//! The transport boundary: whatever actually moves samples between a
//! [`Session`] and the outside world.
//!
//! Two transports share one trait, and the split-screen demo mode is why
//! both exist (see the design spec's "Split-screen demo mode" section):
//!
//! - [`crate::cpal_device::CpalTransport`] drives one [`Session`] against a
//!   real sound card (two machines, the real product) or two `Session`s
//!   against the *same* device pair (`--acoustic` split screen: one
//!   speaker, one microphone, both directions live at once - the hardest
//!   acoustic case there is).
//! - [`WiredTransport`] is the demo default: it hands each `Session`'s
//!   output block straight to the other's input in software - the link
//!   never crosses the air - and mixes both into a real playback stream so
//!   a person listening can still hear the handshake. [`Transport::is_acoustic`]
//!   is `false` for it, and the caller is expected to show `[DEMO MODE]` on
//!   the strength of that, not on any guess about which struct it holds.
//!
//! Tasks 15 and 16 (the TUI and the CLI) consume [`Transport`] as a trait
//! object or a generic parameter and must not know which of the two they
//! were handed - that is the entire point of designing this boundary now
//! rather than retrofitting it once the TUI already has an `if cpal {
//! ... } else { ... }` branch baked into it.
//!
//! # `run` advances by exactly one block, not the whole call
//!
//! Every other streaming type in this workspace - `Tx::read`,
//! `Rx::write`, `Session::process_out`/`process_in` - processes one block
//! per call and expects its caller to loop. [`Transport::run`] keeps that
//! shape rather than blocking for the life of the call: a caller (the TUI's
//! own event loop, eventually) calls `run` once per tick, interleaved with
//! redrawing and reading a keypress, and drives `ends` directly (`send`,
//! `hangup`, `state()`) between calls. This is also what makes `run`
//! testable at all for [`WiredTransport`] without a background thread or
//! any real device: a test's own loop plays the same role the TUI's will.
//!
//! # The real-time discipline
//!
//! `modem-core` is `no_std` specifically so its streaming types cannot
//! allocate, block, or perform I/O in their hot path. A `cpal` callback
//! runs on a real-time OS thread with a hard deadline, and that discipline
//! is worthless if the shim wrapping the core throws it away. So the
//! actual `cpal` callbacks - [`fill_output`] and `cpal_device.rs`'s
//! `drain_input` - touch nothing but a preallocated ring buffer - no
//! `Session`, no allocation, no blocking lock, no `println!`. All of the
//! real processing - `process_out`, `process_in`, the two-end mix -
//! happens inside `run`, on whatever thread calls it, which is free to
//! take its time between one real-time deadline and the next.
//!
//! `fill_output`/`device_err` live here rather than in `cpal_device.rs`
//! because [`WiredTransport`] needs its own output-only stream for
//! playback (see `open_output_stream`) - the one bit of `cpal` this module
//! touches directly - and sharing them is what keeps `CpalTransport`'s own
//! callback wiring identical rather than a second, independently-written
//! copy of the same discipline.

use modem_core::session::Session;
use modem_core::{Config, Duplex, Role};

/// One block, in samples. Chosen to match `session.rs`'s own test
/// convention (`BLOCK = 256`) - about 32 ms at 8 kHz DSP rate or 5.3 ms at
/// 48 kHz device rate, small enough to keep latency reasonable, large
/// enough that a `Vec` sized to it is not reallocated needlessly often by
/// anything upstream.
pub(crate) const BLOCK_LEN: usize = 256;

/// Ring buffer capacity between a real `cpal` callback and `run`'s own
/// processing loop, in samples. Wide enough that a `run` call arriving a
/// little late does not starve the callback mid-block - sixteen blocks of
/// slack - while staying small enough that a genuinely stalled `run` loop
/// produces an audible gap within a fraction of a second rather than a
/// long, silent buffer-draining delay.
pub(crate) const RING_CAPACITY: usize = BLOCK_LEN * 16;

/// What actually moves samples between a [`Session`] and the outside
/// world. See this module's own doc for why two implementations share one
/// trait and why `run` advances by one block rather than the whole call.
pub trait Transport {
    /// Advances this transport, and every [`Session`] in `ends`, by
    /// exactly one block: reads whatever real or software-linked input is
    /// available, feeds it to `process_in`, generates the next block of
    /// `process_out` for each end, and (mixed, if `ends.len() == 2`)
    /// queues it for real playback.
    ///
    /// `ends` must be the length this transport expects -
    /// [`WiredTransport`] always links exactly two; a real
    /// [`crate::cpal_device::CpalTransport`] call takes one end for a
    /// genuine two-machine call or two for `--acoustic` split screen.
    /// Anything else is [`TransportError::WrongEndCount`], not a panic -
    /// a caller mistake here should fail cleanly, the same standard
    /// `Session::send`'s own doc holds itself to.
    fn run(&mut self, ends: &mut [Session]) -> Result<(), TransportError>;

    /// The sample rate every [`Session`] driven by this transport must be
    /// built at. For [`crate::cpal_device::CpalTransport`] this is read
    /// back from the device's actually-negotiated stream configuration,
    /// never the rate requested of it - see `cpal_device.rs`'s own doc for
    /// the bug that guards against.
    fn sample_rate(&self) -> u32;

    /// Whether the link this transport drives genuinely crosses the air.
    /// `false` for [`WiredTransport`] (the two ends are cross-wired in
    /// software) and `true` for [`crate::cpal_device::CpalTransport`],
    /// regardless of how many ends it is driving. A caller shows
    /// `[DEMO MODE]` on the strength of this returning `false`, not on any
    /// guess about which struct it holds.
    fn is_acoustic(&self) -> bool;

    /// Builds a [`Config`] for one end of a call at this transport's own
    /// [`Transport::sample_rate`].
    ///
    /// This is the only way this crate offers to get a `Config` tied to a
    /// transport - there is no parallel path that accepts a caller-supplied
    /// rate - which is what makes the rate-mismatch bug the task brief
    /// names impossible by construction rather than merely discouraged by
    /// convention: "cpal reports the rate it actually got, which may not
    /// be the rate you asked for... A `Session` built at 48000 driven by a
    /// device running at 44100 will decode nothing and look like a DSP
    /// bug." A caller who only ever reaches a `Config` through this method
    /// cannot make that mistake.
    fn config_for(&self, role: Role, duplex: Duplex) -> Config {
        Config {
            sample_rate: self.sample_rate(),
            role,
            duplex,
        }
    }
}

/// Everything that can go wrong building or driving a [`Transport`].
#[derive(Debug)]
pub enum TransportError {
    /// `ends` was not the length this transport requires - see
    /// [`Transport::run`]'s own doc.
    WrongEndCount { expected: &'static str, got: usize },
    /// No output device was found on this host.
    NoOutputDevice,
    /// No input device was found on this host.
    NoInputDevice,
    /// The input device has no configuration able to run at the output
    /// device's negotiated rate. Reported rather than silently building
    /// two `Session`s at two different rates - see this module's doc on
    /// why that is exactly the bug this crate exists to make impossible.
    RateMismatch { output_rate: u32 },
    /// Any other failure `cpal` itself reported, stringified so this type
    /// does not have to track `cpal`'s own, larger error surface.
    Device(String),
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TransportError::WrongEndCount { expected, got } => {
                write!(f, "expected {expected} end(s), got {got}")
            }
            TransportError::NoOutputDevice => write!(f, "no audio output device found"),
            TransportError::NoInputDevice => write!(f, "no audio input device found"),
            TransportError::RateMismatch { output_rate } => write!(
                f,
                "input device has no configuration supporting the output device's negotiated \
                 rate of {output_rate} Hz"
            ),
            TransportError::Device(msg) => write!(f, "audio device error: {msg}"),
        }
    }
}

impl std::error::Error for TransportError {}

/// The split-screen demo's default link: two `Session`s wired directly to
/// each other in software, with the combined signal also sent to a real
/// output device so a person can hear the handshake. See this module's
/// own doc for why the link itself never touches that device.
pub struct WiredTransport {
    sample_rate: u32,
    scratch_a: Vec<f32>,
    scratch_b: Vec<f32>,
    mix: Vec<f32>,
    /// Opened lazily, on the first real `run` call - never touched by
    /// [`WiredTransport::step`], which is what keeps the cross-wiring and
    /// mixing logic testable with no device at all. `None` for the whole
    /// life of an instance that is only ever exercised through `step`
    /// directly, which is exactly how every test in this module uses it.
    playback: Option<(cpal::Stream, rtrb::Producer<f32>)>,
}

impl WiredTransport {
    /// `sample_rate` is the rate the two `Session`s this transport will
    /// drive run their DSP at - a free choice here, not a device
    /// negotiation, because the two ends never touch the real playback
    /// device's samples: they are cross-wired directly in software (see
    /// `step`). A mismatch between this figure and whatever the real
    /// output device `run` eventually opens affects only the pitch of
    /// what comes out of the speakers, never whether the exchanged bytes
    /// are correct - unlike `CpalTransport`, where a rate mismatch is a
    /// protocol bug, not a cosmetic one.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            scratch_a: vec![0.0; BLOCK_LEN],
            scratch_b: vec![0.0; BLOCK_LEN],
            mix: vec![0.0; BLOCK_LEN],
            playback: None,
        }
    }

    /// One block of the wired link, with no device involved at all - this
    /// is the entire behaviour the task brief asks to be tested directly,
    /// separated from `run`'s device-touching wrapper around it.
    ///
    /// `ends[0]`'s `process_out` becomes `ends[1]`'s `process_in` and vice
    /// versa - the software link - and the two rendered blocks are summed
    /// and clamped to `[-1.0, 1.0]` into `self.mix`, the demo's playback
    /// signal. Uses this transport's own preallocated `scratch_a`,
    /// `scratch_b` and `mix` every call - never a fresh `Vec` - which is
    /// what `step_does_not_allocate_after_construction` checks directly.
    fn step(&mut self, ends: &mut [Session]) {
        assert_eq!(
            ends.len(),
            2,
            "WiredTransport always links exactly two ends"
        );
        let (first, second) = ends.split_at_mut(1);
        let a = &mut first[0];
        let b = &mut second[0];
        a.process_out(&mut self.scratch_a);
        b.process_out(&mut self.scratch_b);
        a.process_in(&self.scratch_b);
        b.process_in(&self.scratch_a);
        for i in 0..BLOCK_LEN {
            self.mix[i] = (self.scratch_a[i] + self.scratch_b[i]).clamp(-1.0, 1.0);
        }
    }
}

impl Transport for WiredTransport {
    fn run(&mut self, ends: &mut [Session]) -> Result<(), TransportError> {
        if ends.len() != 2 {
            return Err(TransportError::WrongEndCount {
                expected: "2 (WiredTransport always links exactly two ends)",
                got: ends.len(),
            });
        }
        self.step(ends);
        if self.playback.is_none() {
            self.playback = Some(open_output_stream(self.sample_rate)?);
        }
        // Unwrap is safe: the line above just set it on the only path
        // that could have left it `None`.
        let (_stream, producer) = self.playback.as_mut().expect("just set above");
        for &s in &self.mix {
            // A full ring buffer means the real device has fallen behind;
            // dropping the sample is the right call, not blocking this
            // (non-real-time) loop on a device that may never catch up.
            let _ = producer.push(s);
        }
        Ok(())
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn is_acoustic(&self) -> bool {
        false
    }
}

/// Opens the default output device, best-effort at `wanted` Hz, purely for
/// [`WiredTransport`]'s own playback - the mix a person listens to, never
/// anything fed back into a `Session`. A mismatch between `wanted` and
/// what the device actually negotiates only changes the pitch of what
/// comes out of the speakers, which is why this function - unlike
/// `CpalTransport::new` - does not treat that as an error: `WiredTransport`'s
/// two `Session`s never touch this device's samples, so there is no
/// protocol correctness for a rate mismatch here to break.
pub(crate) fn open_output_stream(
    wanted: u32,
) -> Result<(cpal::Stream, rtrb::Producer<f32>), TransportError> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(TransportError::NoOutputDevice)?;
    let mut config = device.default_output_config().map_err(device_err)?.config();

    let device_supports_wanted = device
        .supported_output_configs()
        .map(|mut configs| {
            configs.any(|c| c.min_sample_rate() <= wanted && wanted <= c.max_sample_rate())
        })
        .unwrap_or(false);
    if device_supports_wanted {
        config.sample_rate = wanted;
    }

    let channels = config.channels as usize;
    let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(RING_CAPACITY);

    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| fill_output(data, &mut consumer, channels),
            |err| eprintln!("modem-audio: output stream error: {err}"),
            None,
        )
        .map_err(device_err)?;
    stream.play().map_err(device_err)?;

    Ok((stream, producer))
}

/// The output stream's entire callback body - shared by [`WiredTransport`]'s
/// own playback stream and `cpal_device.rs`'s `CpalTransport`. Copies one
/// queued mono sample per output frame into every channel `cpal` actually
/// asked for. Never allocates - `data` is the caller's own slice and
/// `consumer`'s backing storage is fixed at construction - and fills any
/// shortfall (the ring buffer running dry) with silence rather than
/// waiting for `run` to catch up, which is the one thing a real-time
/// callback must never do.
pub(crate) fn fill_output(data: &mut [f32], consumer: &mut rtrb::Consumer<f32>, channels: usize) {
    for frame in data.chunks_mut(channels.max(1)) {
        let sample = consumer.pop().unwrap_or(0.0);
        for s in frame.iter_mut() {
            *s = sample;
        }
    }
}

/// Converts any `cpal` failure into a [`TransportError::Device`],
/// stringified so this crate's own error type does not have to track
/// `cpal`'s larger error surface. Shared with `cpal_device.rs`.
pub(crate) fn device_err(e: cpal::Error) -> TransportError {
    TransportError::Device(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use modem_core::session::SessionState;

    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    fn full_duplex_cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::Full,
        }
    }

    /// Pumps `ends` through `WiredTransport::step` until both report
    /// `Connected`, mirroring `session.rs`'s own `connect` helper exactly,
    /// but through this module's entry point instead of hand-wiring
    /// `process_out`/`process_in`.
    fn connect(transport: &mut WiredTransport, ends: &mut [Session]) {
        for _ in 0..4000 {
            transport.step(ends);
            if ends.iter().all(|s| s.state() == SessionState::Connected) {
                return;
            }
        }
        panic!(
            "sessions never both reached Connected: {:?}",
            ends.iter().map(Session::state).collect::<Vec<_>>()
        );
    }

    fn settle(transport: &mut WiredTransport, ends: &mut [Session]) {
        for _ in 0..10 {
            transport.step(ends);
        }
    }

    /// Required test: `WiredTransport` carries a payload between two
    /// sessions, byte-exact. Mirrors `session.rs`'s own
    /// `two_sessions_exchange_data_in_both_directions`, but driven through
    /// `step` rather than a hand-rolled pump, so this proves the new
    /// entry point behaves identically to the already-proven manual
    /// wiring, not a second, independent claim about `Session` itself.
    ///
    /// Checks content, not aggregates - `receive()` compared against the
    /// literal bytes sent, per this project's own established convention.
    #[test]
    fn wired_transport_carries_a_payload_between_two_sessions_byte_exact() {
        let mut originate = Session::new(cfg(Role::Originate));
        let mut answer = Session::new(cfg(Role::Answer));
        originate.dial("1");
        answer.answer();

        let mut transport = WiredTransport::new(8000);
        let mut ends = [originate, answer];
        connect(&mut transport, &mut ends);
        settle(&mut transport, &mut ends);

        ends[0].send(b"HELLO ANSWER");
        for _ in 0..500 {
            transport.step(&mut ends);
        }
        assert_eq!(ends[1].receive(), b"HELLO ANSWER");
    }

    // Mutation 1 target: dropping one direction of the cross-wire (e.g.
    // commenting out `b.process_in(&self.scratch_a)`) must fail the test
    // above - see the task report for the actual failure it produces. No
    // separate test is needed here: the payload test above already
    // requires both directions - originate must both dial *and* be heard
    // - to reach `Connected` at all, so a one-direction cut fails it at
    // `connect`, before `send` is ever called.

    /// Required test: the playback mix is non-silent while either end is
    /// transmitting.
    ///
    /// Checks content, not an aggregate "any nonzero" scan: `step` already
    /// hands the caller `self.scratch_a`/`self.scratch_b` (via `ends`'
    /// own state is not enough - see below) - this test independently
    /// recomputes the expected mix from what `process_out` actually wrote
    /// this same block and asserts `step`'s `mix` matches it exactly, at
    /// every sample. A mutation that filled `mix` with silence fails this
    /// immediately (mutation 2); so does a subtler one that copied only
    /// one side into `mix` and dropped the other - an "any nonzero" check
    /// cannot tell that apart from a correct mix, because one side alone
    /// is still nonzero, which is exactly the aggregate-blind shape this
    /// project's mutation proofs warn about.
    ///
    /// `Duplex::Full` is used specifically so both ends are genuinely
    /// transmitting their idle-mark tone at the same instant once
    /// connected - `session.rs`'s own doc: idle mark is a real transmitted
    /// tone, not silence - which `Duplex::HalfPingPong` would not
    /// guarantee (the end without the turn transmits exact silence, so a
    /// "drop one side" mutation would be invisible there: the dropped
    /// side is already all zero).
    #[test]
    fn playback_mix_is_non_silent_while_either_end_is_transmitting() {
        let mut originate = Session::new(full_duplex_cfg(Role::Originate));
        let mut answer = Session::new(full_duplex_cfg(Role::Answer));
        originate.dial("1");
        answer.answer();

        let mut transport = WiredTransport::new(8000);
        let mut ends = [originate, answer];
        connect(&mut transport, &mut ends);

        transport.step(&mut ends);

        assert!(
            transport.scratch_a.iter().any(|&x| x != 0.0),
            "precondition failed: originate produced no signal to mix"
        );
        assert!(
            transport.scratch_b.iter().any(|&x| x != 0.0),
            "precondition failed: answer produced no signal to mix"
        );
        assert!(
            transport.mix.iter().any(|&x| x != 0.0),
            "playback mix was silent while both ends were transmitting"
        );
        for i in 0..BLOCK_LEN {
            let expected = (transport.scratch_a[i] + transport.scratch_b[i]).clamp(-1.0, 1.0);
            assert_eq!(
                transport.mix[i], expected,
                "mix[{i}] did not match the sum of both ends' own output"
            );
        }
    }

    /// Required test: `is_acoustic()` is false for wired and true for
    /// cpal. The `cpal` half is proven in `cpal_device.rs`'s own test
    /// module, against a real `CpalTransport` built through its test-only
    /// constructor (a genuine device cannot be opened in CI - see that
    /// module's doc).
    #[test]
    fn is_acoustic_is_false_for_wired() {
        let wired = WiredTransport::new(48000);
        assert!(!wired.is_acoustic());
    }

    struct FakeTransport {
        rate: u32,
    }

    impl Transport for FakeTransport {
        fn run(&mut self, _ends: &mut [Session]) -> Result<(), TransportError> {
            Ok(())
        }
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn is_acoustic(&self) -> bool {
            false
        }
    }

    /// Required test: a rate mismatch is impossible by construction.
    /// `config_for` is the only way this crate offers to build a `Config`
    /// tied to a transport, and it always reads `sample_rate()` - this
    /// proves it does, rather than (mutation 3) a hardcoded figure such as
    /// the 48000 the task brief names.
    #[test]
    fn config_for_uses_the_transports_own_rate_not_a_hardcoded_one() {
        let transport = FakeTransport { rate: 44100 };
        let cfg = transport.config_for(Role::Answer, Duplex::Full);
        assert_eq!(
            cfg.sample_rate, 44100,
            "config_for did not use the transport's own reported rate"
        );
    }

    /// Own mutation (5): `config_for` could satisfy the test above by
    /// reading `sample_rate()` correctly while still hardcoding `role` or
    /// `duplex` - the required test checks only one of the three fields
    /// `Config` actually has, exactly the aggregate-blind shape this
    /// project's mutation proofs warn about (a single-field check standing
    /// in for "the whole struct is right"). This checks the other two
    /// independently, with a role and duplex chosen to disagree with
    /// `Config`'s field order and any plausible default, so a hardcoded
    /// `Role::Originate` or `Duplex::HalfPingPong` would be caught rather
    /// than coincide with the values under test.
    #[test]
    fn config_for_passes_through_role_and_duplex_unchanged() {
        let transport = FakeTransport { rate: 8000 };
        let cfg = transport.config_for(Role::Answer, Duplex::Full);
        assert_eq!(
            cfg.role,
            Role::Answer,
            "config_for did not pass role through"
        );
        assert_eq!(
            cfg.duplex,
            Duplex::Full,
            "config_for did not pass duplex through"
        );
    }

    /// Required test: no allocation in the hot path, checked after every
    /// call rather than only first vs last - `tx.rs`'s own test found the
    /// system allocator's free list ping-pongs between two addresses on
    /// repeated same-size alloc-then-free, so an endpoint-only comparison
    /// can pass against precisely the bug it exists to catch. This checks
    /// all three of `step`'s owned scratch buffers, every call.
    #[test]
    fn step_does_not_allocate_after_construction() {
        let mut originate = Session::new(full_duplex_cfg(Role::Originate));
        let mut answer = Session::new(full_duplex_cfg(Role::Answer));
        originate.dial("1");
        answer.answer();

        let mut transport = WiredTransport::new(8000);
        let mut ends = [originate, answer];
        // One call first, matching tx.rs's own convention: the pointer is
        // captured only after construction is fully behind us, so a
        // one-time lazy-init allocation on the very first call is not
        // mistaken for the hot-path bug this test exists to catch.
        transport.step(&mut ends);
        let a_ptr = transport.scratch_a.as_ptr();
        let b_ptr = transport.scratch_b.as_ptr();
        let mix_ptr = transport.mix.as_ptr();

        for i in 0..50 {
            transport.step(&mut ends);
            assert_eq!(
                transport.scratch_a.as_ptr(),
                a_ptr,
                "scratch_a moved at call {i}"
            );
            assert_eq!(
                transport.scratch_b.as_ptr(),
                b_ptr,
                "scratch_b moved at call {i}"
            );
            assert_eq!(transport.mix.as_ptr(), mix_ptr, "mix moved at call {i}");
        }
    }
}
