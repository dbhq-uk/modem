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
    ///
    /// The input range is also filtered to ones that can deliver `f32`
    /// (every `SupportedInputConfigs` entry pins one native sample
    /// format, and a real interface routinely exposes several side by
    /// side - confirmed by hand: this crate always builds an `f32`
    /// stream regardless of which range gets picked, so a range whose
    /// only fault is a different native format is exactly as unusable as
    /// a rate mismatch, not a detail `build_input_stream` can quietly
    /// paper over), then - among those - prefers one whose channel count
    /// matches the output's own, since some real duplex devices (also
    /// confirmed by hand) reject a channel-count mismatch between their
    /// own playback and capture sides even when each count is
    /// independently listed as supported.
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

        let input_range = choose_input_range(
            input_device.supported_input_configs().map_err(device_err)?,
            sample_rate,
            output_channels,
        )
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

/// Picks which of `candidates` (the input device's own reported ranges)
/// [`CpalTransport::new`] should build its input stream from, given the
/// output device's already-negotiated `sample_rate` and `output_channels`.
/// A standalone function - rather than inline in `new` - specifically so
/// this selection can be unit-tested with synthetic ranges: `new` itself
/// can never run in CI (see this module's doc), so without this split the
/// logic a real hand-verification found broken would stay untested.
///
/// Two rules, in order:
/// 1. Only a range that can deliver `f32` at `sample_rate` is even a
///    candidate - `build_input_stream::<f32, _, _>` always asks for `f32`
///    regardless of which range gets picked, so a range whose only fault
///    is a different native sample format (common on real interfaces,
///    which often expose several side by side - confirmed by hand: a
///    real duplex device's own capture side enumerated i16/i24/i32/f32
///    variants together) must never win by simply appearing first. The
///    version this replaced picked by rate alone and failed with an
///    opaque `UnsupportedConfig` from `build_input_stream` itself.
/// 2. Among the `f32` candidates, prefer one whose channel count matches
///    `output_channels` - confirmed by hand against a real duplex device
///    that rejects a channel-count mismatch between its own playback and
///    capture sides even though each count is independently listed as
///    supported - falling back to whichever `f32` candidate comes first
///    when no such match exists, since plenty of real mic/speaker pairs
///    (a mono microphone feeding a stereo speaker, say) have no reason to
///    agree on channels at all.
fn choose_input_range(
    candidates: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
    sample_rate: u32,
    output_channels: usize,
) -> Option<cpal::SupportedStreamConfigRange> {
    let f32_candidates: Vec<_> = candidates
        .filter(|c| {
            c.sample_format() == cpal::SampleFormat::F32
                && c.min_sample_rate() <= sample_rate
                && sample_rate <= c.max_sample_rate()
        })
        .collect();
    f32_candidates
        .iter()
        .find(|c| c.channels() as usize == output_channels)
        .or(f32_candidates.first())
        .cloned()
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

    fn range(
        channels: u16,
        min_rate: u32,
        max_rate: u32,
        format: cpal::SampleFormat,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            min_rate,
            max_rate,
            cpal::SupportedBufferSize::Range { min: 1, max: 4096 },
            format,
        )
    }

    /// Mutation-proof for the actual hand-verification finding: a real
    /// duplex device (confirmed with `snd-aloop`) enumerated an i16 mono
    /// range *before* its f32 stereo range for the same rate. Picking by
    /// rate alone (the version this replaced) chose the i16 range and
    /// `build_input_stream::<f32, _, _>` then failed with an opaque
    /// `UnsupportedConfig` - reproduced here with synthetic ranges in the
    /// same order, no real device required.
    #[test]
    fn choose_input_range_skips_a_non_f32_range_even_when_it_comes_first() {
        let candidates = vec![
            range(1, 8000, 192000, cpal::SampleFormat::I16),
            range(2, 8000, 192000, cpal::SampleFormat::F32),
        ];
        let chosen = choose_input_range(candidates.into_iter(), 48000, 2)
            .expect("an f32 candidate exists and must be chosen");
        assert_eq!(
            chosen.sample_format(),
            cpal::SampleFormat::F32,
            "chose a range that cannot deliver f32, which build_input_stream::<f32, _, _> always asks for"
        );
    }

    /// The other half: among several f32-capable ranges, the one whose
    /// channel count matches the output's own must win, even when a
    /// different channel count is listed first - confirmed by hand
    /// against a real duplex device that rejected the mismatched-channel
    /// choice with the same opaque `UnsupportedConfig`, despite each
    /// count being independently listed as supported.
    #[test]
    fn choose_input_range_prefers_a_channel_count_matching_the_output() {
        let candidates = vec![
            range(1, 8000, 192000, cpal::SampleFormat::F32),
            range(2, 8000, 192000, cpal::SampleFormat::F32),
            range(3, 8000, 192000, cpal::SampleFormat::F32),
        ];
        let chosen = choose_input_range(candidates.into_iter(), 48000, 2)
            .expect("a channel-matching f32 candidate exists and must be chosen");
        assert_eq!(
            chosen.channels(),
            2,
            "did not prefer the range whose channel count matches the output's own"
        );
    }

    /// When no f32 candidate's channel count matches the output's own,
    /// falling back to the first f32 candidate is still correct - many
    /// real mic/speaker pairs (a mono microphone feeding a stereo
    /// speaker) have no reason to agree on channels at all, so this must
    /// not be treated as failure.
    #[test]
    fn choose_input_range_falls_back_to_first_f32_candidate_when_no_channel_match_exists() {
        let candidates = vec![
            range(1, 8000, 192000, cpal::SampleFormat::F32),
            range(4, 8000, 192000, cpal::SampleFormat::F32),
        ];
        let chosen = choose_input_range(candidates.into_iter(), 48000, 2)
            .expect("an f32 candidate exists even with no channel match, and must still be chosen");
        assert_eq!(
            chosen.channels(),
            1,
            "did not fall back to the first f32 candidate when no channel count matched"
        );
    }

    /// No candidate at all - wrong rate, wrong format, or an empty
    /// device - must report `None` rather than panicking, so `new` can
    /// turn it into a clean `TransportError::RateMismatch`.
    #[test]
    fn choose_input_range_returns_none_when_nothing_matches() {
        let candidates = vec![
            range(2, 8000, 44100, cpal::SampleFormat::F32),
            range(2, 8000, 192000, cpal::SampleFormat::I16),
        ];
        assert!(
            choose_input_range(candidates.into_iter(), 48000, 2).is_none(),
            "returned a candidate despite none supporting f32 at the requested rate"
        );
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
    ///
    /// `run`'s input and output loops can now each iterate zero, one or
    /// many times within a *single* call (see this module's doc) - the
    /// scratch-reuse discipline matters most exactly inside that loop, so
    /// this drains a varying number of whole blocks (1, then 2, then 1,
    /// then 2, ...) every call, rather than feeding a fixed sub-block
    /// amount once up front, so every one of the 50 checked calls actually
    /// exercises more than one inner iteration at least half the time.
    /// `played` is drained every call too, so the output loop keeps
    /// finding room and is exercised the same way, not just the input one.
    #[test]
    fn run_does_not_allocate_after_construction() {
        let (mut transport, mut mic, mut played) = CpalTransport::new_for_test(8000, false);
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
            // Alternate one and two whole blocks' worth of freshly
            // "captured" audio so the input loop's iteration count varies
            // call to call - see this test's own doc on why a fixed count
            // would not be enough to trust the check below.
            let blocks = if i % 2 == 0 { 1 } else { 2 };
            for _ in 0..(BLOCK_LEN * blocks) {
                let _ = mic.push(0.1);
            }
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
            // Keep the output ring drained so the next call's output loop
            // has room to run more than once too.
            while played.pop().is_ok() {}
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

    /// Mutation-proof for the actual defect this fix closes. Queues fewer
    /// than one whole block of real "captured" audio, then calls `run`
    /// once, and checks the *ring's own remaining content* - not just the
    /// returned `RunStats` - which is what a mutation could otherwise
    /// fake. Two independent facts must both hold: `run` did no input
    /// work (`input_blocks == 0`) and every one of the real samples is
    /// still sitting in the ring, untouched, for the next call.
    ///
    /// The old, reverted design failed this immediately: it popped
    /// exactly `BLOCK_LEN` samples every call regardless of how many were
    /// actually queued, substituting `0.0` for the shortfall via
    /// `unwrap_or(0.0)`. That would have drained all
    /// `BLOCK_LEN - 1` real samples here (leaving the ring empty, not
    /// holding `BLOCK_LEN - 1`) and spliced one fabricated silent sample
    /// onto the end of them before ever reporting anything back - exactly
    /// the corruption that broke the answer end's Gardner timing loop and
    /// carrier detector on the real `snd-aloop` loopback. Reintroducing
    /// that old body (restoring the fixed-`BLOCK_LEN`,
    /// `unwrap_or(0.0)` loop in place of the `while ... slots()` loop
    /// above) was verified by hand to fail both assertions below - see
    /// the task report.
    #[test]
    fn run_does_not_fabricate_a_partial_block_when_less_than_one_is_queued() {
        let (mut transport, mut mic, _played) = CpalTransport::new_for_test(8000, false);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::Full));
        originate.dial("1");
        let mut ends = [originate];

        let queued = BLOCK_LEN - 1;
        for _ in 0..queued {
            let _ = mic.push(0.3);
        }

        let stats = transport.run(&mut ends).unwrap();

        assert_eq!(
            stats.input_blocks, 0,
            "run() processed a block before a whole block's worth of real input had arrived"
        );
        assert_eq!(
            transport.input_consumer.slots(),
            queued,
            "run() consumed real captured samples without a whole block being available - \
             they must stay queued for the next call, not be dropped or padded with silence"
        );
    }

    /// The other half of the same mutation-proof: when *several* whole
    /// blocks are genuinely queued at once (a caller that briefly fell
    /// behind the real device), a single `run` call must drain all of
    /// them, not just one. Checked the same way - both the returned count
    /// and the ring's own remaining content, so a mutation cannot satisfy
    /// one while faking the other.
    ///
    /// The old, reverted one-block-per-call design left `BLOCK_LEN * 2`
    /// samples still queued after this call (it only ever drained one
    /// block, no matter how many were waiting) - verified by hand; see
    /// the task report.
    #[test]
    fn run_drains_every_whole_block_available_in_one_call() {
        let (mut transport, mut mic, _played) = CpalTransport::new_for_test(8000, false);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::Full));
        originate.dial("1");
        let mut ends = [originate];

        let whole_blocks = 3;
        for _ in 0..(BLOCK_LEN * whole_blocks) {
            let _ = mic.push(0.0);
        }

        let stats = transport.run(&mut ends).unwrap();

        assert_eq!(
            stats.input_blocks, whole_blocks,
            "run() did not drain every whole block queued in a single call"
        );
        assert_eq!(
            transport.input_consumer.slots(),
            0,
            "whole blocks were left queued in the ring instead of being drained this call"
        );
    }

    /// The output side of the same fix: `run` must not blindly push a
    /// block into an already-full output ring - it has to check for room
    /// first and simply do nothing once the ring cannot take a whole
    /// block, the same discipline the input loop above is held to.
    /// Checked against the ring's own occupancy (`slots()`), not just the
    /// returned count, for the same reason as the two tests above.
    #[test]
    fn run_stops_producing_output_once_the_ring_is_full() {
        let (mut transport, _mic, _played) = CpalTransport::new_for_test(8000, false);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::Full));
        originate.dial("1");
        let mut ends = [originate];

        // Nobody ever drains `_played`, so the very first call already
        // fills the output ring completely (its capacity is a whole
        // number of blocks - see `RING_CAPACITY`'s own doc).
        let first = transport.run(&mut ends).unwrap();
        assert!(
            first.output_blocks > 0,
            "precondition failed: the first call produced no output at all"
        );
        assert_eq!(
            transport.output_producer.slots(),
            0,
            "precondition failed: the output ring was not actually filled"
        );

        let second = transport.run(&mut ends).unwrap();
        assert_eq!(
            second.output_blocks, 0,
            "run() produced output into a ring that already had no room for a whole block"
        );
    }

    /// Required by the brief: samples `drain_input` genuinely drops (the
    /// real-time capture callback finding the ring already full) must be
    /// reported, not silently absorbed - the whole point of this fix.
    /// Calls `drain_input` directly with a ring left with no room at all,
    /// the same function a real `cpal` input stream callback runs, then
    /// checks `run`'s own `RunStats::input_samples_dropped` picks up
    /// exactly that count on its very next call. This is the one loss
    /// `run`'s own drain-what's-available redesign above cannot prevent -
    /// it happens on the real-time thread before `run` is ever called -
    /// so reporting it, rather than eliminating it, is the correct fix.
    #[test]
    fn dropped_input_samples_are_reported_via_run_stats() {
        let (mut transport, mut mic, _played) = CpalTransport::new_for_test(8000, false);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::Full));
        originate.dial("1");
        let mut ends = [originate];

        // Fill the input ring completely first...
        while mic.push(0.0).is_ok() {}
        // ...then simulate the real-time capture callback trying to add
        // five more samples than the ring has room for. `drain_input`
        // takes a `&mut rtrb::Producer`, the same handle a real `cpal`
        // input stream owns - `mic` here plays exactly that role.
        let dropped_before = transport.input_dropped.load(Ordering::Relaxed);
        drain_input(
            &[0.1, 0.2, 0.3, 0.4, 0.5],
            &mut mic,
            1,
            &transport.input_dropped,
        );
        assert_eq!(
            transport.input_dropped.load(Ordering::Relaxed) - dropped_before,
            5,
            "precondition failed: drain_input did not count the samples it had to drop"
        );

        let stats = transport.run(&mut ends).unwrap();
        assert_eq!(
            stats.input_samples_dropped, 5,
            "run() did not surface drain_input's real dropped-sample count via RunStats"
        );

        // And the counter must not double-report the same drops on a
        // second call with nothing new dropped in between.
        let stats2 = transport.run(&mut ends).unwrap();
        assert_eq!(
            stats2.input_samples_dropped, 0,
            "run() re-reported the same drop on a later call instead of reporting only the delta"
        );
    }

    /// The acceptance test's in-memory analogue: two `Session`s sharing
    /// one `CpalTransport`, driven through a caller loop whose call rate
    /// has no relationship to the device's own - the actual free-running-
    /// caller-clock scenario this module's own doc describes, reproduced
    /// deterministically with no real device, no real time, and no
    /// flakiness.
    ///
    /// The "device" here advances by exactly one `BLOCK_LEN` period every
    /// tick, always, independent of how many times (zero, one, or several)
    /// the caller happens to call `run` that same tick: it pops up to one
    /// block from `played` (an underrun - less than a block queued - is
    /// padded with real silence, exactly what a real DAC does when
    /// starved) and feeds exactly that much straight back into `mic`, a
    /// same-machine loopback with no acoustic loss. This is the crucial
    /// difference from an earlier, discarded version of this test: a
    /// "device" that only ever fed back what the caller had *just*
    /// produced could never race ahead of or fall behind the caller, so it
    /// could not reproduce the mismatch at all (verified by hand: that
    /// version passed even with `run` reverted to its old defective body).
    /// Decoupling the device's own per-tick advance from `CALL_PATTERN`
    /// below is what makes the two clocks genuinely independent, the same
    /// way a real sound card's clock never waits for the caller's.
    ///
    /// `CALL_PATTERN` includes a `0` (the caller falls behind a device
    /// tick entirely) and repeated multiples (the caller then races back
    /// past it) specifically so both directions of the mismatch the task
    /// brief names are exercised, not just one.
    ///
    /// Asserts on a decoded, byte-exact payload - never on reaching
    /// `Connected` alone, which the task brief's own hand-verification
    /// showed proves nothing about the link (a session built at the wrong
    /// rate for its transport still reached `Connected` in 9.0 s on real
    /// hardware; the overture that gets a session to `Connected` never
    /// decodes anything).
    ///
    /// Reverting `run` to its old fixed-one-block, `unwrap_or(0.0)` body
    /// was verified by hand to fail this test (payload not received
    /// within the tick budget below) - see the task report.
    #[test]
    fn two_sessions_exchange_a_payload_over_a_simulated_loopback_with_irregular_call_pacing() {
        use modem_core::session::SessionState;

        let (mut transport, mut mic, mut played) = CpalTransport::new_for_test(8000, true);
        let mut originate = Session::new(cfg(Role::Originate, Duplex::HalfPingPong));
        let mut answer = Session::new(cfg(Role::Answer, Duplex::HalfPingPong));
        originate.dial("1");
        answer.answer();
        let mut ends = [originate, answer];

        // How many times the caller calls `run` on a given tick, cycled -
        // deliberately including a tick where it does not call at all
        // (falling a whole device period behind) followed by ticks that
        // call several times in a row (racing back past it), so both
        // directions of the mismatch get exercised many times over the
        // run below, not just once.
        const CALL_PATTERN: [usize; 5] = [0, 3, 1, 0, 2];
        let mut connected_at = None;

        for tick in 0..60_000 {
            let calls = CALL_PATTERN[tick % CALL_PATTERN.len()];
            for _ in 0..calls {
                transport.run(&mut ends).unwrap();
            }

            // The device: exactly one BLOCK_LEN period this tick, always,
            // whether or not the caller called `run` at all. Pops up to
            // one block from `played`; an underrun (less queued than a
            // whole block) is padded with real silence, then fed back -
            // a real DAC does not wait for more samples to arrive either.
            let mut period = [0.0f32; BLOCK_LEN];
            for slot in period.iter_mut() {
                *slot = played.pop().unwrap_or(0.0);
            }
            for &s in &period {
                let _ = mic.push(s);
            }

            if connected_at.is_none()
                && ends[0].state() == SessionState::Connected
                && ends[1].state() == SessionState::Connected
            {
                connected_at = Some(tick);
            }
            if let Some(connected_tick) = connected_at {
                // A short settle after connecting (matching every other
                // exchange test in this workspace's own convention) before
                // sending, then give the payload a generous window to
                // round-trip.
                if tick == connected_tick + 400 {
                    ends[0].send(b"IRREGULAR PACING");
                }
                if tick > connected_tick + 400 {
                    let got = ends[1].receive();
                    if !got.is_empty() {
                        assert_eq!(
                            got, b"IRREGULAR PACING",
                            "payload arrived corrupted under irregular call pacing"
                        );
                        return;
                    }
                }
            }
        }

        panic!(
            "payload never round-tripped under irregular call pacing within the tick budget - \
             final states: originate={:?} answer={:?}",
            ends[0].state(),
            ends[1].state()
        );
    }
}
