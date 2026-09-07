//! Streaming FSK modulator.

use alloc::collections::VecDeque;
#[cfg(test)]
use alloc::vec;
use alloc::vec::Vec;

use crate::frame::frame_byte;
use crate::nco::Nco;
use crate::{samples_per_symbol, tones, Config, DSP_RATE};

/// Write queues bytes; read fills sample blocks. Stateful: the symbol clock
/// and the oscillator phase both carry across calls, so blocks must be read
/// in order.
///
/// The scratch buffer is owned and grown once, never per call. Allocating
/// inside an audio callback is how you get dropouts.
pub struct Tx {
    cfg: Config,
    nco: Nco,
    mark: f64,
    space: f64,
    bits: VecDeque<bool>,
    sym_pos: f64,
    current: bool,
    scratch: Vec<f64>,
}

impl Tx {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.sample_rate > 0, "sample_rate must be greater than zero");
        let (mark, space) = tones(cfg.role);
        Self {
            cfg,
            nco: Nco::new(mark, DSP_RATE),
            mark,
            space,
            bits: VecDeque::new(),
            sym_pos: 0.0,
            current: true, // idle holds mark
            scratch: Vec::new(),
        }
    }

    pub fn write(&mut self, data: &[u8]) {
        for &b in data {
            self.bits.extend(frame_byte(b));
        }
    }

    pub fn pending(&self) -> bool {
        !self.bits.is_empty()
    }

    /// How many samples `n` bits occupy. Used by the handshake sequencer and
    /// by tests to lay out timed stages exactly.
    pub fn samples_for_bits(&self, n: usize) -> usize {
        libm::round(n as f64 * samples_per_symbol()) as usize
    }

    /// Fills `out` with the next block at the device rate, holding mark when
    /// idle. Allocates only on the first call, or if the block size grows.
    pub fn read(&mut self, out: &mut [f32]) {
        let need = libm::ceil(out.len() as f64 * DSP_RATE / self.cfg.sample_rate as f64) as usize;
        if self.scratch.len() < need {
            self.scratch.resize(need, 0.0);
        }

        // Derived from samples_per_symbol so there is exactly one
        // implementation of the 26.6667 arithmetic. A second inline copy
        // would not be covered by that function's test, and a rounding
        // regression here would ship green.
        let step = 1.0 / samples_per_symbol();

        for i in 0..need {
            self.scratch[i] = self.nco.next();
            self.sym_pos += step;
            if self.sym_pos >= 1.0 {
                self.sym_pos -= 1.0;
                self.next_symbol();
            }
        }
        crate::resample::to_device(
            &self.scratch[..need],
            out,
            DSP_RATE,
            self.cfg.sample_rate as f64,
        );
    }

    fn next_symbol(&mut self) {
        self.current = self.bits.pop_front().unwrap_or(true); // idle mark
        self.nco
            .set_freq(if self.current { self.mark } else { self.space });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nco::goertzel;
    use crate::{Duplex, Role};

    fn cfg(role: Role) -> Config {
        Config {
            sample_rate: 8000,
            role,
            duplex: Duplex::HalfPingPong,
        }
    }

    fn f64s(v: &[f32]) -> Vec<f64> {
        v.iter().map(|&x| x as f64).collect()
    }

    #[test]
    fn tones_per_role() {
        assert_eq!(tones(Role::Originate), (1270.0, 1070.0));
        assert_eq!(tones(Role::Answer), (2225.0, 2025.0));
    }

    #[test]
    fn idle_transmitter_holds_mark() {
        let mut tx = Tx::new(cfg(Role::Originate));
        let mut buf = vec![0.0f32; 8000];
        tx.read(&mut buf);
        assert!(goertzel(&f64s(&buf), 1270.0, 8000.0) >= 0.9);
    }

    /// The critical timing property. Over 1000 bits, rounding to 27 samples
    /// per symbol puts the last bit 333 samples late - more than twelve
    /// whole symbols.
    #[test]
    fn samples_for_bits_is_fractional() {
        let tx = Tx::new(cfg(Role::Originate));
        assert_eq!(tx.samples_for_bits(1000), 26_667);
    }

    /// Guards the live path. samples_for_bits has its own test above, but
    /// read() carries its own symbol accumulator and only this covers it.
    /// Mutation-proven against a rounded step.
    #[test]
    fn read_symbol_clock_drains_bits_at_the_right_rate() {
        let mut tx = Tx::new(cfg(Role::Originate));
        const CHARS: usize = 200;
        tx.write(&[0x55; CHARS]);
        let queued = CHARS * 10;

        let total = tx.samples_for_bits(queued);
        let mut buf = vec![0.0f32; total];
        tx.read(&mut buf);

        let drained = queued - tx.bits.len();
        assert!(
            drained >= queued - 5,
            "drained {drained} of {queued} queued bits, symbol clock is drifting"
        );
    }

    #[test]
    fn write_then_read_produces_both_tones() {
        let mut tx = Tx::new(cfg(Role::Originate));
        tx.write(&[0x00]);
        let mut buf = vec![0.0f32; 800];
        tx.read(&mut buf);
        let s = f64s(&buf);
        assert!(goertzel(&s, 1270.0, 8000.0) >= 0.2, "no mark tone");
        assert!(goertzel(&s, 1070.0, 8000.0) >= 0.2, "no space tone");
    }

    #[test]
    fn read_does_not_allocate_after_the_first_call() {
        let mut tx = Tx::new(cfg(Role::Originate));
        let mut buf = vec![0.0f32; 512];
        tx.read(&mut buf);
        let cap = tx.scratch.capacity();
        for _ in 0..50 {
            tx.read(&mut buf);
        }
        assert_eq!(
            tx.scratch.capacity(),
            cap,
            "scratch reallocated in the hot path"
        );
    }
}
