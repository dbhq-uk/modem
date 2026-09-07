//! 8-N-1 asynchronous framing. `true` is mark, which is binary 1.

/// One byte as ten bits: a space start bit, eight data bits
/// least-significant first, then a mark stop bit.
pub fn frame_byte(b: u8) -> [bool; 10] {
    let mut bits = [false; 10];
    bits[0] = false; // start bit is always space
    for i in 0..8 {
        bits[1 + i] = b & (1 << i) != 0;
    }
    bits[9] = true; // stop bit is always mark
    bits
}

/// Reassembles bytes from a sliced bit stream. Hunts for a start bit while
/// idle, takes the next eight bits as data, then checks the stop bit.
///
/// A framing error is counted, not returned. At 300 baud over air the right
/// response is to carry on and let the character land as a typo rather than
/// tear the connection down.
///
/// This only detects corruption *within* an aligned frame. If the bit clock
/// slips - a bit inserted or dropped - the deframer will silently emit
/// fabricated bytes with no error signalled. Preventing that is the
/// demodulator's job, not this one's.
#[derive(Default)]
pub struct Deframer {
    in_frame: bool,
    bit_count: u8,
    acc: u8,
    errors: usize,
}

impl Deframer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn framing_errors(&self) -> usize {
        self.errors
    }

    pub fn push_bit(&mut self, mark: bool) -> Option<u8> {
        if !self.in_frame {
            if !mark {
                // a space while idle is a start bit
                self.in_frame = true;
                self.bit_count = 0;
                self.acc = 0;
            }
            return None;
        }

        if self.bit_count < 8 {
            if mark {
                self.acc |= 1 << self.bit_count;
            }
            self.bit_count += 1;
            return None;
        }

        // stop bit position
        self.in_frame = false;
        if !mark {
            self.errors += 1;
            return None;
        }
        Some(self.acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_shape_and_bit_order() {
        let bits = frame_byte(0x41); // 'A' = 0100 0001
        assert!(!bits[0], "start bit must be space");
        assert!(bits[9], "stop bit must be mark");
        // LSB first
        let want = [true, false, false, false, false, false, true, false];
        assert_eq!(&bits[1..9], &want);
    }

    /// 0x5A is bit-palindromic, so a single-value test cannot detect a
    /// reversed bit order. This one can.
    #[test]
    fn bit_order_is_lsb_first_not_msb() {
        assert_eq!(
            &frame_byte(0x01)[1..9],
            &[true, false, false, false, false, false, false, false]
        );
        assert_eq!(
            &frame_byte(0x80)[1..9],
            &[false, false, false, false, false, false, false, true]
        );
    }

    #[test]
    fn deframer_ignores_idle_then_recovers_a_byte() {
        let mut d = Deframer::new();
        for _ in 0..20 {
            assert_eq!(d.push_bit(true), None, "emitted a byte while idle");
        }
        let mut got = None;
        for b in frame_byte(0x5A) {
            got = d.push_bit(b);
        }
        assert_eq!(got, Some(0x5A));
    }

    #[test]
    fn deframer_round_trips_all_byte_values() {
        let mut d = Deframer::new();
        for v in 0u16..256 {
            let mut got = None;
            for b in frame_byte(v as u8) {
                got = d.push_bit(b);
            }
            assert_eq!(got, Some(v as u8), "value {v:#04x} failed");
            d.push_bit(true); // idle between frames
        }
    }

    #[test]
    fn deframer_counts_a_bad_stop_bit() {
        let mut d = Deframer::new();
        d.push_bit(false);
        for _ in 0..8 {
            d.push_bit(true);
        }
        d.push_bit(false); // stop bit is space - a framing error
        assert_eq!(d.framing_errors(), 1);
    }

    /// After a corrupted frame the deframer must pick the stream back up.
    /// Task 5 feeds it a noisy real-world bit stream.
    #[test]
    fn deframer_resynchronises_after_a_framing_error() {
        let mut d = Deframer::new();
        d.push_bit(false);
        for _ in 0..8 {
            d.push_bit(true);
        }
        d.push_bit(false); // bad stop bit
        assert_eq!(d.framing_errors(), 1);

        d.push_bit(true); // idle
        let mut got = None;
        for b in frame_byte(0x42) {
            got = d.push_bit(b);
        }
        assert_eq!(
            got,
            Some(0x42),
            "did not resynchronise after a framing error"
        );
    }
}
