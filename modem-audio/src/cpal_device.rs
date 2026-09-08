//! The real duplex sound-card backend: [`CpalTransport`] drives a genuine
//! input and output stream pair. [`fill_output`](crate::transport::fill_output)
//! (the output callback's entire body) and the rate-negotiation error
//! conversion live in `transport.rs` instead, shared with
//! [`crate::transport::WiredTransport`]'s own output-only playback stream -
//! see that module's doc for why. This file adds only what a genuine
//! *duplex* device needs on top: a real input stream, and the one shared
//! rate the two must agree on.
//!
//! # cpal reports the rate it actually got
//!
//! A caller can ask a device for 48000 Hz and be handed back a stream
//! running at 44100 Hz - the hardware, the OS mixer, or another
//! application already holding the device all get a vote. This module
//! never trusts the rate it asked for: [`CpalTransport::new`] reads the
//! rate back from the *built* stream configuration, stores it, and
//! [`Transport::sample_rate`] returns that figure and nothing else. A
//! `Session` built at the wrong rate does not error - it just decodes
//! nothing, and looks exactly like a DSP bug while you are looking in
//! entirely the wrong file for it. See `transport.rs`'s `config_for`,
//! which is the only path this crate offers for turning a transport into
//! a `Session`'s `Config`, for how that mistake is made impossible rather
//! than merely documented against.
//!
//! # The callback touches nothing but a ring buffer
//!
//! `fill_output` and `drain_input` below are the *entire* body of the
//! real-time callbacks `cpal` calls on its own audio thread. Both are
//! plain functions, independent of `cpal` itself, so they can be (and
//! are, in this module's own tests) exercised with no real device at all.
//! Neither allocates, blocks, nor touches a `Session` - all of the actual
//! DSP work (`process_in`, `process_out`, the two-end mix) happens inside
//! [`CpalTransport::run`], called from whatever thread the caller runs its
//! own loop on, which - unlike the real-time callback - is free to take
//! its time. This is the split the task brief asks for: "keep
//! `CpalTransport` thin enough that its correctness is obvious by
//! inspection" applies to `fill_output`/`drain_input` and the plumbing in
//! `new`; `run`'s own processing loop is exactly as testable as
//! `WiredTransport::step`, and is tested the same way, via a device-free
//! constructor (see `new_for_test`).
//!
//! # `run` is now paced by the device's ring occupancy, not a fixed block
//!
//! Hand-verification against a real `snd-aloop` loopback (see the task
//! report for Task 13's follow-up) found two `Session`s sharing one
//! `CpalTransport` never connecting, with continuous buffer
//! underrun/overrun on both real streams. The cause: `run` used to pop
//! *exactly* `BLOCK_LEN` samples from `input_consumer` every call,
//! substituting `0.0` for any shortfall, and generate *exactly* one block
//! of output every call, regardless of how much room the output ring
//! actually had. A caller paced by `sleep(BLOCK / sample_rate)` has no way
//! to stay exactly in step with the real device clock, so that fixed
//! figure was always either too much (fabricating silence into real
//! captured audio, corrupting the exact sample sequence the Gardner timing
//! loop and carrier detector depend on) or too little (leaving real
//! captured samples queued until the input ring filled and `drain_input`
//! started dropping newly-arrived ones for real).
//!
//! `run` now loops on `input_consumer.slots()`/`output_producer.slots()`:
//! it drains every whole block genuinely queued (zero, one, or as many as
//! the ring holds) and generates output for every whole block of room
//! genuinely available, then stops - never waiting, never padding a
//! partial block with fabricated samples. A caller running slightly slow
//! catches up on its very next call; one running fast simply finds
//! nothing to do. [`crate::transport::RunStats`] reports how many blocks a
//! call actually processed, and `input_dropped` (below) counts samples
//! genuinely lost in `drain_input` itself - the one loss this design
//! cannot eliminate, because it happens on the real-time thread before
//! `run` is ever called, but can now report rather than hide.
//!
//! # Real audio cannot be tested in CI
//!
//! No test in this module ever calls [`CpalTransport::new`] - it opens a
//! genuine device, which CI does not have (confirmed directly on the
//! machine this task was built on: no ALSA device nodes, no loadable
//! `snd-dummy`/`snd-aloop` kernel modules, nothing under `/proc/asound`).
//! `new_for_test` builds the same struct with in-memory ring buffers and
//! no `cpal::Stream` at all, which is enough to test every line of `run`'s
//! own logic - only `new`'s device-opening path is unverified by the test
//! suite, and that path is deliberately small. See the task report for
//! what was verified by hand instead.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use modem_core::session::Session;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::transport::{
    device_err, fill_output, RunStats, Transport, TransportError, BLOCK_LEN, RING_CAPACITY,
};

/// Drives one [`Session`] against a real sound card (a genuine
/// two-machine call) or two against the same device pair (`--acoustic`
/// split screen - one microphone, one speaker, both directions live).
/// See this module's own doc for the rate-negotiation and real-time
/// discipline this type exists to enforce.
pub struct CpalTransport {
    sample_rate: u32,
    acoustic: bool,
    /// Held only for RAII - dropping a `cpal::Stream` stops it. Never
    /// read after construction.
    _output_stream: Option<cpal::Stream>,
    _input_stream: Option<cpal::Stream>,
    output_producer: rtrb::Producer<f32>,
    input_consumer: rtrb::Consumer<f32>,
    scratch_in: Vec<f32>,
    scratch_out_a: Vec<f32>,
    scratch_out_b: Vec<f32>,
    /// Cumulative count of captured samples `drain_input` (the real-time
    /// capture callback) has ever had to drop because the input ring was
    /// already full. Shared with that callback via `Arc` so incrementing
    /// it there is a single lock-free atomic add - no allocation, no
    /// blocking, the same real-time discipline as the ring buffer itself.
    /// `run` reads it each call and reports the *delta* since the
    /// previous call as `RunStats::input_samples_dropped`, via
    /// `input_dropped_baseline` below.
    input_dropped: Arc<AtomicU64>,
    /// The value of `input_dropped` as of the end of the previous `run`
    /// call - see its own doc.
    input_dropped_baseline: u64,
}

impl CpalTransport {
    /// Opens the default input and output devices and negotiates one
    /// shared rate between them. `acoustic` records whether this instance
    /// is being asked to drive the hardest case - two ends sharing one
    /// microphone and one speaker - purely so [`Transport::is_acoustic`]
    /// can report it; it does not change how the streams are built.
    ///
    /// The output device's own default configuration decides the rate
    /// (`sample_rate`, below); the input device is then required to
    /// support that *same* rate explicitly, rather than negotiating its
    /// own default and trusting the two to agree - seeing them disagree
    /// here, loudly, as [`TransportError::RateMismatch`], is the whole
    /// point: it is a real, reportable failure, not something to paper
    /// over by building two `Session`s at two different rates.
    pub fn new(acoustic: bool) -> Result<Self, TransportError> {
        let host = cpal::default_host();
        let output_device = host
            .default_output_device()
            .ok_or(TransportError::NoOutputDevice)?;
        let input_device = host
            .default_input_device()
            .ok_or(TransportError::NoInputDevice)?;

        let output_supported = output_device.default_output_config().map_err(device_err)?;
        let sample_rate = output_supported.sample_rate();
        let output_config = output_supported.config();
        let output_channels = output_config.channels as usize;

        let input_range = input_device
            .supported_input_configs()
            .map_err(device_err)?
            .find(|c| c.min_sample_rate() <= sample_rate && sample_rate <= c.max_sample_rate())
            .ok_or(TransportError::RateMismatch {
                output_rate: sample_rate,
            })?;
        let input_config = input_range.with_sample_rate(sample_rate).config();
        let input_channels = input_config.channels as usize;

        let (output_producer, mut output_consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);
        let (mut input_producer, input_consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);
        let input_dropped = Arc::new(AtomicU64::new(0));
        let input_dropped_for_callback = Arc::clone(&input_dropped);

        let output_stream = output_device
            .build_output_stream::<f32, _, _>(
                output_config,
                move |data: &mut [f32], _| fill_output(data, &mut output_consumer, output_channels),
                |err| eprintln!("modem-audio: output stream error: {err}"),
                None,
            )
            .map_err(device_err)?;

        let input_stream = input_device
            .build_input_stream::<f32, _, _>(
                input_config,
                move |data: &[f32], _| {
                    drain_input(
                        data,
                        &mut input_producer,
                        input_channels,
                        &input_dropped_for_callback,
                    )
                },
                |err| eprintln!("modem-audio: input stream error: {err}"),
                None,
            )
            .map_err(device_err)?;

        output_stream.play().map_err(device_err)?;
        input_stream.play().map_err(device_err)?;

        Ok(Self {
            sample_rate,
            acoustic,
            _output_stream: Some(output_stream),
            _input_stream: Some(input_stream),
            output_producer,
            input_consumer,
            scratch_in: vec![0.0; BLOCK_LEN],
            scratch_out_a: vec![0.0; BLOCK_LEN],
            scratch_out_b: vec![0.0; BLOCK_LEN],
            input_dropped,
            input_dropped_baseline: 0,
        })
    }

    /// Builds a `CpalTransport` with no real device at all: in-memory ring
    /// buffers standing in for the ones a real stream's callback would
    /// drain and fill. This is what lets `run`'s own processing logic -
    /// draining captured input, calling `process_in`/`process_out`, mixing
    /// two ends' output - be tested in CI, which has no audio hardware.
    ///
    /// Returns the transport plus the *other* half of each ring buffer: a
    /// producer a test can push fake "captured" samples into, and a
    /// consumer it can pop the transport's own queued playback samples
    /// back out of.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        sample_rate: u32,
        acoustic: bool,
    ) -> (Self, rtrb::Producer<f32>, rtrb::Consumer<f32>) {
        let (output_producer, output_consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);
        let (input_producer, input_consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);
        let transport = Self {
            sample_rate,
            acoustic,
            _output_stream: None,
            _input_stream: None,
            output_producer,
            input_consumer,
            scratch_in: vec![0.0; BLOCK_LEN],
            scratch_out_a: vec![0.0; BLOCK_LEN],
            scratch_out_b: vec![0.0; BLOCK_LEN],
            input_dropped: Arc::new(AtomicU64::new(0)),
            input_dropped_baseline: 0,
        };
        (transport, input_producer, output_consumer)
    }
}

impl Transport for CpalTransport {
    /// See this module's own doc ("`run` is now paced by the device's ring
    /// occupancy, not a fixed block") for the defect this replaced and
    /// why. Two loops, each bounded by the ring's own fixed capacity so
    /// neither can spin unboundedly or block:
    ///
    /// - Drains every whole block of real captured input actually queued
    ///   (`input_consumer.slots() >= BLOCK_LEN`), feeding each one to
    ///   every end in `ends` in arrival order, before generating anything.
    ///   A remainder smaller than one block is left queued rather than
    ///   padded with fabricated silence - `pop()` is only ever called
    ///   after `slots()` has already confirmed a whole block is there, so
    ///   it cannot itself fall back to `0.0`.
    /// - Generates and queues output only while the output ring has room
    ///   for a whole block (`output_producer.slots() >= BLOCK_LEN`) -
    ///   catching the ring up when there is slack, and doing no work at
    ///   all when it is already full, rather than blindly pushing one more
    ///   block regardless of whether anything will ever drain it.
    fn run(&mut self, ends: &mut [Session]) -> Result<RunStats, TransportError> {
        if ends.is_empty() || ends.len() > 2 {
            return Err(TransportError::WrongEndCount {
                expected: "1 (a real two-machine call) or 2 (--acoustic split screen)",
                got: ends.len(),
            });
        }

        let mut input_blocks = 0usize;
        while self.input_consumer.slots() >= BLOCK_LEN {
            for slot in self.scratch_in.iter_mut() {
                *slot = self
                    .input_consumer
                    .pop()
                    .expect("slots() just confirmed a whole block is queued");
            }
            // Both ends hear the same microphone in the two-end
            // (`--acoustic`) case - there is only one, physically.
            for end in ends.iter_mut() {
                end.process_in(&self.scratch_in);
            }
            input_blocks += 1;
        }

        let mut output_blocks = 0usize;
        while self.output_producer.slots() >= BLOCK_LEN {
            if ends.len() == 1 {
                ends[0].process_out(&mut self.scratch_out_a);
                for &s in &self.scratch_out_a {
                    let _ = self.output_producer.push(s);
                }
            } else {
                let (first, second) = ends.split_at_mut(1);
                first[0].process_out(&mut self.scratch_out_a);
                second[0].process_out(&mut self.scratch_out_b);
                for i in 0..self.scratch_out_a.len() {
                    let mixed = (self.scratch_out_a[i] + self.scratch_out_b[i]).clamp(-1.0, 1.0);
                    let _ = self.output_producer.push(mixed);
                }
            }
            output_blocks += 1;
        }

        let dropped_total = self.input_dropped.load(Ordering::Relaxed);
        let input_samples_dropped = dropped_total.wrapping_sub(self.input_dropped_baseline);
        self.input_dropped_baseline = dropped_total;

        Ok(RunStats {
            input_blocks,
            output_blocks,
            input_samples_dropped,
        })
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn is_acoustic(&self) -> bool {
        self.acoustic
    }
}

/// The input stream's entire callback body, the same discipline as
/// [`fill_output`](crate::transport::fill_output). Downmixes to mono by
/// averaging - the same convention `wav::read_wav` already uses - and
/// drops a captured sample the ring buffer has no room for rather than
/// blocking the capture thread until `run` makes room, but - unlike the
/// version this replaced - counts every one it drops in `dropped` first.
/// A single lock-free atomic add: no allocation, no blocking, so the
/// real-time discipline this callback must keep is unaffected. `run`
/// reads `dropped` itself and turns it into `RunStats::input_samples_dropped`
/// - see this module's own doc.
fn drain_input(
    data: &[f32],
    producer: &mut rtrb::Producer<f32>,
    channels: usize,
    dropped: &AtomicU64,
) {
    let channels = channels.max(1);
    for frame in data.chunks(channels) {
        let mono = frame.iter().sum::<f32>() / frame.len() as f32;
        if producer.push(mono).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modem_core::{Config, Duplex, Role};

    fn cfg(role: Role, duplex: Duplex) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex,
        }
    }

    /// Required test: `is_acoustic()` is true for cpal (and false for
    /// wired - proven in `transport.rs`'s own test module). Built through
    /// `new_for_test`, since a real device cannot be opened in CI - see
    /// this module's doc.
    #[test]
    fn is_acoustic_is_true_for_cpal() {
        let (transport, _mic, _played) = CpalTransport::new_for_test(48000, true);
        assert!(transport.is_acoustic());
    }

    /// `sample_rate()` reports exactly the figure the transport was built
    /// with - `new_for_test` stands in for "the rate `new` read back from
    /// the real device", and this proves `sample_rate()` is not, say,
    /// hardcoded to some common default that would coincidentally match a
    /// real device's usual 44100 or 48000 during manual testing.
    #[test]
    fn sample_rate_reports_the_negotiated_figure() {
        let (transport, _mic, _played) = CpalTransport::new_for_test(44100, false);
        assert_eq!(transport.sample_rate(), 44100);
    }

    /// Required test / mutation 4 target: no allocation in the callback
    /// path. `fill_output`/`drain_input` themselves own no resizable
    /// buffer at all (by design - see this module's doc), so the
    /// meaningful check is on `run`'s own owned scratch, which is exactly
    /// where a batching loop would actually grow a `Vec` if it stopped
    /// reusing one. Checked after every call, not just first vs last -
    /// `tx.rs`'s own test found the allocator's free list ping-pongs
    /// between two addresses on repeated same-size alloc-then-free, so an
    /// endpoint-only comparison can pass against precisely the bug it
    /// exists to catch.
    #[test]
    fn run_does_not_allocate_after_construction() {
        let (mut transport, mut mic, _played) = CpalTransport::new_for_test(8000, false);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::Full));
        originate.dial("1");
        let mut ends = [originate];

        // One call first (matching tx.rs's own convention), then feed a
        // little real "captured" audio so the input path is genuinely
        // exercised, not just falling through the empty-buffer branch.
        transport.run(&mut ends).unwrap();
        for _ in 0..64 {
            let _ = mic.push(0.1);
        }
        let in_ptr = transport.scratch_in.as_ptr();
        let out_ptr = transport.scratch_out_a.as_ptr();

        for i in 0..50 {
            transport.run(&mut ends).unwrap();
            assert_eq!(
                transport.scratch_in.as_ptr(),
                in_ptr,
                "scratch_in moved at call {i}"
            );
            assert_eq!(
                transport.scratch_out_a.as_ptr(),
                out_ptr,
                "scratch_out_a moved at call {i}"
            );
        }
    }

    /// `run` rejects the wrong number of ends cleanly rather than
    /// panicking on an out-of-bounds index inside its own `ends.len() ==
    /// 1` / `else` split.
    #[test]
    fn run_rejects_zero_or_more_than_two_ends() {
        let (mut transport, _mic, _played) = CpalTransport::new_for_test(8000, false);
        assert!(matches!(
            transport.run(&mut []),
            Err(TransportError::WrongEndCount { got: 0, .. })
        ));

        let a = Session::new(cfg(Role::Originate, Duplex::Full));
        let b = Session::new(cfg(Role::Answer, Duplex::Full));
        let c = Session::new(cfg(Role::Answer, Duplex::Full));
        assert!(matches!(
            transport.run(&mut [a, b, c]),
            Err(TransportError::WrongEndCount { got: 3, .. })
        ));
    }

    /// In `--acoustic` split-screen mode both ends genuinely share one
    /// microphone, so `run` must feed the *same* captured block to every
    /// end in `ends`, not only the first. A mutation that fed just
    /// `ends[0]` would leave `ends[1]` unable to ever raise carrier no
    /// matter what arrives - caught here, at the second end specifically,
    /// where such a bug would actually hide (checking only `ends[0]`, the
    /// one every plausible partial-distribution bug still updates
    /// correctly, is exactly the aggregate-blind shape this project's
    /// mutation proofs warn about).
    #[test]
    fn acoustic_mode_feeds_the_shared_microphone_to_every_end() {
        use modem_core::tx::Tx;

        let (mut transport, mut mic, _played) = CpalTransport::new_for_test(8000, true);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::HalfPingPong));
        let mut answer = Session::new(cfg(Role::Answer, Duplex::HalfPingPong));
        originate.dial("1");
        answer.answer();
        let mut ends = [originate, answer];

        assert!(
            !ends[1].carrier_detected(),
            "carrier detected before anything was fed in"
        );

        // A real Originate-band tone - what `answer`'s Rx (tones(Answer.
        // listen()) == tones(Originate)) needs to raise carrier.
        let mut tx = Tx::new(cfg(Role::Originate, Duplex::HalfPingPong));
        let mut tone = vec![0.0f32; 8000];
        tx.read(&mut tone);
        for &s in &tone {
            let _ = mic.push(s);
        }

        for _ in 0..(tone.len() / BLOCK_LEN + 1) {
            transport.run(&mut ends).unwrap();
        }

        assert!(
            ends[1].carrier_detected(),
            "the second end never saw the shared microphone's input"
        );
    }
}
