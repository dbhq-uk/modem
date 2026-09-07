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
}
