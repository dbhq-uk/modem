//! The packet layer above the raw 8-N-1 byte stream.
//!
//! `frame.rs` frames characters and detects nothing: a plausible wrong
//! byte, a lost byte and a duplicated byte all look like data to it. This
//! layer adds a sync byte, byte stuffing, a CRC, a sequence number and a
//! turn token for half duplex arbitration - the same job HDLC-style
//! framing does in real modems.
//!
//! # Wire format
//!
//! ```text
//! 0x7E  seq  kind  length  payload...  crc_hi  crc_lo
//! ^sync  \_______________ byte-stuffed ________________/
//! ```
//!
//! The sync byte `0x7E` is never itself stuffed - it is the one byte the
//! receiver can always trust as a frame boundary, and that is what makes
//! resynchronisation after junk or a corrupt frame possible at all.
//! Everything after it - `seq`, `kind`, `length`, the payload, and both
//! CRC bytes - is byte-stuffed as one run: any byte equal to `0x7E` or the
//! escape byte `0x7D` is replaced by `0x7D` followed by that byte XORed
//! with `0x20`. The CRC bytes are included deliberately, not as an
//! afterthought - a CRC that happened to produce a literal `0x7E` would
//! corrupt framing exactly like an unstuffed payload byte if it were left
//! out (see `crc_bytes_that_collide_with_framing_bytes_are_also_stuffed`
//! below, which exercises a real CRC output containing `0x7E`).
//!
//! `length` is the payload's length before stuffing, `0..=240`
//! ([`MAX_PAYLOAD`]). The CRC is CRC-16/CCITT-FALSE (polynomial `0x1021`,
//! initial value `0xFFFF`, no reflection, no final XOR) over the unstuffed
//! `seq, kind, length, payload` bytes, sent high byte first.
//!
//! # Chat characters ride without retry, deliberately
//!
//! A corrupted character in a live chat should appear as a typo - that is
//! what the era felt like, and retrying it would be a lie about the
//! medium. Reliable transfer is a different protocol's job. This layer
//! only refuses to hand a corrupt *packet* to the application; it does
//! not retransmit.
//!
//! # The self-consistency trap
//!
//! Every test in this file encodes with [`encode_packet`] and decodes
//! with [`PacketReader`]. That loop is exactly what let this project's Go
//! attempt ship a reversed bit order, and what nearly let this codebase
//! ship a non-functional timing loop, while every test passed. Two
//! defences are load-bearing here, not decorative: the CRC-16/CCITT-FALSE
//! check vector (`crc_matches_the_standard_check_vector`), which is a
//! published fact this code cannot silently agree with itself about, and
//! [`wire_format_matches_a_hand_computed_frame`] plus two further tests
//! below, each a literal byte array computed independently of this file's
//! own encoder.

use alloc::vec::Vec;

/// Marks the start of every packet. Never stuffed - the receiver relies
/// on this byte being unambiguous to resynchronise after junk or a
/// corrupt frame.
const SYNC: u8 = 0x7E;

/// Introduces a stuffed byte. The byte that follows it is the original
/// byte XORed with [`ESC_XOR`].
const ESC: u8 = 0x7D;

const ESC_XOR: u8 = 0x20;

/// Largest payload [`encode_packet`] accepts. Chosen so `length` always
/// fits in one byte with headroom to spare, not because 240 is otherwise
/// significant.
pub const MAX_PAYLOAD: usize = 240;

/// CRC-16/CCITT-FALSE: polynomial `0x1021`, initial value `0xFFFF`, no
/// reflection, no final XOR.
///
/// Pinned by the standard check vector in this module's tests: the CRC of
/// the ASCII string `123456789` must be `0x29B1`. A wrong initial value,
/// a reflected bit order, or a stray final XOR are all self-consistent -
/// encode and decode would agree with each other and every round-trip
/// test would still pass - which is exactly why that vector, not a
/// round-trip, is what pins this function.
fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// What a packet carries. `Data` and `Ack` are the ordinary link traffic;
/// `Turn` carries no payload and exists purely to hand the other end
/// permission to transmit on a half duplex link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketKind {
    Data,
    Ack,
    Turn,
}

impl PacketKind {
    /// The wire value for this kind. Written out as an explicit match,
    /// not an `as u8` cast on the discriminant, so the mapping is a
    /// visible, testable decision rather than a fact about enum layout.
    fn to_byte(self) -> u8 {
        match self {
            PacketKind::Data => 0,
            PacketKind::Ack => 1,
            PacketKind::Turn => 2,
        }
    }

    /// The inverse of [`PacketKind::to_byte`]. `None` for any value none
    /// of the three kinds use, which `PacketReader` treats as a corrupt
    /// frame rather than guessing.
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(PacketKind::Data),
            1 => Some(PacketKind::Ack),
            2 => Some(PacketKind::Turn),
            _ => None,
        }
    }
}

/// One packet: a sequence number, a kind, and a payload (empty for
/// [`PacketKind::Turn`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Packet {
    pub seq: u8,
    pub kind: PacketKind,
    pub payload: Vec<u8>,
}

/// Encodes `pkt` into the wire format described in this module's doc:
/// sync byte, then the byte-stuffed `seq, kind, length, payload, crc`.
///
/// Panics if `pkt.payload.len()` exceeds [`MAX_PAYLOAD`] - a caller
/// assembling a payload larger than the wire format can express is a
/// programming error to catch here, not a value to silently truncate or
/// smuggle through in pieces.
pub fn encode_packet(pkt: &Packet) -> Vec<u8> {
    let len = pkt.payload.len();
    assert!(
        len <= MAX_PAYLOAD,
        "payload length {len} exceeds the {MAX_PAYLOAD}-byte maximum"
    );

    let mut body = Vec::with_capacity(3 + len + 2);
    body.push(pkt.seq);
    body.push(pkt.kind.to_byte());
    body.push(len as u8);
    body.extend_from_slice(&pkt.payload);

    let crc = crc16_ccitt_false(&body);
    body.push((crc >> 8) as u8);
    body.push(crc as u8);

    let mut out = Vec::with_capacity(1 + body.len());
    out.push(SYNC);
    for &b in &body {
        if b == SYNC || b == ESC {
            out.push(ESC);
            out.push(b ^ ESC_XOR);
        } else {
            out.push(b);
        }
    }
    out
}

/// Where a [`PacketReader`] is within the byte stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum ReadState {
    /// Discarding bytes until the next sync byte.
    #[default]
    Hunting,
    /// Accumulating a frame's unstuffed body.
    InFrame,
}

/// Reassembles [`Packet`]s from a byte stream that may arrive in any
/// chunking - a whole packet at once, one byte at a time, or with junk
/// bytes in between. Feed it bytes with [`PacketReader::push`]; it hands
/// back every complete, CRC-valid packet found so far.
///
/// A frame that fails its CRC, or whose `kind` byte matches none of
/// [`PacketKind`]'s values, is silently dropped - not returned, not
/// counted here. The reader then keeps hunting for the next sync byte,
/// which is what lets a corrupt frame or a run of line noise be skipped
/// without wedging the stream. A literal `0x7E` byte always starts a new
/// frame immediately, even mid-frame: that is what makes the sync byte a
/// reliable resynchronisation point, and it correctly discards a partial
/// frame that a real sync byte for the next packet has already begun to
/// overtake.
#[derive(Default)]
pub struct PacketReader {
    state: ReadState,
    /// `true` immediately after an unprocessed escape byte.
    escaped: bool,
    /// Unstuffed bytes accumulated for the frame currently in progress:
    /// `seq, kind, length, payload..., crc_hi, crc_lo` once complete.
    body: Vec<u8>,
    /// Total bytes `body` needs before the frame is complete. Known only
    /// once the `length` byte (the third body byte) has arrived.
    expected_len: Option<usize>,
}

impl PacketReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds `bytes` into the reader and returns every packet that
    /// became complete during this call, in order. May return zero, one,
    /// or several packets; state persists across calls, so a packet can
    /// be split across any number of `push` calls at any byte boundary.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Packet> {
        let mut out = Vec::new();
        for &b in bytes {
            if let Some(pkt) = self.push_byte(b) {
                out.push(pkt);
            }
        }
        out
    }

    fn push_byte(&mut self, b: u8) -> Option<Packet> {
        if b == SYNC {
            self.state = ReadState::InFrame;
            self.escaped = false;
            self.body.clear();
            self.expected_len = None;
            return None;
        }

        if self.state != ReadState::InFrame {
            return None; // junk before the first sync byte
        }

        if self.escaped {
            self.escaped = false;
            self.body.push(b ^ ESC_XOR);
        } else if b == ESC {
            self.escaped = true;
            return None;
        } else {
            self.body.push(b);
        }

        if self.expected_len.is_none() && self.body.len() >= 3 {
            let payload_len = self.body[2] as usize;
            self.expected_len = Some(3 + payload_len + 2);
        }

        let total = self.expected_len?;
        if self.body.len() < total {
            return None;
        }

        // The frame is complete either way past this point - reset for
        // the next one regardless of whether it checks out, so a corrupt
        // frame cannot wedge the reader against real data behind it.
        let crc_at = total - 2;
        let calc = crc16_ccitt_false(&self.body[..crc_at]);
        let got = ((self.body[crc_at] as u16) << 8) | self.body[crc_at + 1] as u16;
        let result = if calc == got {
            PacketKind::from_byte(self.body[1]).map(|kind| Packet {
                seq: self.body[0],
                kind,
                payload: self.body[3..crc_at].to_vec(),
            })
        } else {
            None
        };

        self.state = ReadState::Hunting;
        self.body.clear();
        self.expected_len = None;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn crc_matches_the_standard_check_vector() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    }

    /// A literal byte array with the CRC written out by hand (verified
    /// independently against the check vector above, not against this
    /// file's own encoder), so the wire format is pinned to something
    /// other than this code's opinion of it. This also pins `seq` and
    /// `kind` to distinct wire offsets with distinct values (0x01 and
    /// 0x00) - a decoder that read them from swapped offsets would need
    /// this exact byte sequence to change, and it would not.
    #[test]
    fn wire_format_matches_a_hand_computed_frame() {
        let pkt = Packet {
            seq: 0x01,
            kind: PacketKind::Data,
            payload: vec![0x41, 0x42],
        };
        let encoded = encode_packet(&pkt);
        assert_eq!(
            encoded,
            vec![0x7E, 0x01, 0x00, 0x02, 0x41, 0x42, 0x83, 0x46]
        );
    }

    /// `PacketKind`'s wire byte for each of the three kinds, asserted
    /// directly against `encode_packet`'s output - not round-tripped
    /// through `PacketReader`. A `to_byte`/`from_byte` pair that
    /// consistently swapped, say, `Ack` and `Turn` would pass every
    /// round-trip test in this file; it cannot pass this one, because
    /// this test never calls the decoder that would swap them back.
    #[test]
    fn kind_byte_values_are_pinned_to_the_wire() {
        let encode = |kind| {
            encode_packet(&Packet {
                seq: 0,
                kind,
                payload: Vec::new(),
            })
        };
        assert_eq!(encode(PacketKind::Data)[2], 0x00);
        assert_eq!(encode(PacketKind::Ack)[2], 0x01);
        assert_eq!(encode(PacketKind::Turn)[2], 0x02);
    }

    /// `seq`, `kind` and `length` at their documented offsets, using
    /// three different values so a swap among them changes the outcome
    /// rather than being invisible. Own mutation proof: swapping the
    /// order these three fields are written in - while keeping encode
    /// and decode mutually consistent - passes every round-trip test in
    /// this file (that is the self-consistency trap this task names
    /// explicitly), but changes the byte at one of these three fixed
    /// positions, which this test checks without ever calling
    /// `PacketReader`.
    #[test]
    fn seq_kind_and_length_occupy_the_documented_wire_offsets() {
        let encoded = encode_packet(&Packet {
            seq: 0x42,
            kind: PacketKind::Ack,
            payload: Vec::new(),
        });
        assert_eq!(encoded[1], 0x42, "seq must be the first body byte");
        assert_eq!(encoded[2], 0x01, "kind must be the second body byte");
        assert_eq!(encoded[3], 0x00, "length must be the third body byte");
    }

    #[test]
    fn round_trip_an_ordinary_payload() {
        let pkt = Packet {
            seq: 7,
            kind: PacketKind::Data,
            payload: b"hello".to_vec(),
        };
        let encoded = encode_packet(&pkt);
        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&encoded), vec![pkt]);
    }

    /// The payload deliberately contains the sync byte, the escape byte,
    /// and the escape byte's own XORed form (`0x7D ^ 0x20 = 0x5D`) - the
    /// last one checks that the reader tracks real escape *state* rather
    /// than treating every `0x5D` byte as needing to be un-XORed, which
    /// would corrupt a payload byte that merely happens to look like a
    /// stuffed escape's second half.
    ///
    /// The expected bytes are hand-computed (body
    /// `[0x02, 0x00, 0x03, 0x7E, 0x7D, 0x5D]`, CRC `0x28D8`): the two
    /// framing bytes each expand into a two-byte escape pair, the `0x5D`
    /// passes through unstuffed, and this is checked as a literal array
    /// before the round trip runs at all.
    #[test]
    fn payload_containing_sync_and_escape_bytes_is_stuffed_and_round_trips() {
        let payload = vec![0x7E, 0x7D, 0x7D ^ 0x20];
        let pkt = Packet {
            seq: 0x02,
            kind: PacketKind::Data,
            payload: payload.clone(),
        };
        let encoded = encode_packet(&pkt);
        assert_eq!(
            encoded,
            vec![0x7E, 0x02, 0x00, 0x03, 0x7D, 0x5E, 0x7D, 0x5D, 0x5D, 0x28, 0xD8]
        );

        let mut reader = PacketReader::new();
        let got = reader.push(&encoded);
        assert_eq!(got, vec![pkt]);
    }

    /// `seq=0x00, kind=Ack, payload=[0x41, 0x42, 0x00]` produces a CRC of
    /// `0x7EAE` - its own high byte collides with the sync byte. Hand
    /// computed (body `0001034142 00`, CRC `0x7EAE`) to confirm the CRC
    /// field is included in the stuffed run, not appended raw after it;
    /// if it were appended raw, this exact frame would contain a literal
    /// `0x7E` in the middle of the stream and could not be told apart
    /// from the start of a new packet.
    #[test]
    fn crc_bytes_that_collide_with_framing_bytes_are_also_stuffed() {
        let payload = vec![0x41, 0x42, 0x00];
        let pkt = Packet {
            seq: 0x00,
            kind: PacketKind::Ack,
            payload: payload.clone(),
        };
        let encoded = encode_packet(&pkt);
        assert_eq!(
            encoded,
            vec![0x7E, 0x00, 0x01, 0x03, 0x41, 0x42, 0x00, 0x7D, 0x5E, 0xAE]
        );

        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&encoded), vec![pkt]);
    }

    /// Flips a bit in a payload byte and leaves every framing byte -
    /// sync, seq, kind, length and both CRC bytes - untouched.
    #[test]
    fn a_corrupted_payload_byte_is_rejected() {
        let pkt = Packet {
            seq: 3,
            kind: PacketKind::Data,
            payload: b"hello".to_vec(),
        };
        let mut encoded = encode_packet(&pkt);
        let payload_start = 1 + 3; // sync, seq, kind, length
        encoded[payload_start] ^= 0x01;

        let mut reader = PacketReader::new();
        let got = reader.push(&encoded);
        assert!(got.is_empty(), "a corrupted packet was delivered anyway");
    }

    #[test]
    fn a_packet_split_one_byte_at_a_time_is_reassembled() {
        let pkt = Packet {
            seq: 9,
            kind: PacketKind::Ack,
            payload: b"ok".to_vec(),
        };
        let encoded = encode_packet(&pkt);

        let mut reader = PacketReader::new();
        let mut got = Vec::new();
        for &b in &encoded {
            got.extend(reader.push(&[b]));
        }
        assert_eq!(got, vec![pkt]);
    }

    #[test]
    fn a_turn_token_survives_the_round_trip() {
        let pkt = Packet {
            seq: 0,
            kind: PacketKind::Turn,
            payload: Vec::new(),
        };
        let encoded = encode_packet(&pkt);
        // Turn's own wire value, pinned directly, not just round-tripped.
        assert_eq!(encoded[2], 0x02);

        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&encoded), vec![pkt]);
    }

    #[test]
    fn resynchronises_after_junk_with_no_sync_byte() {
        let pkt = Packet {
            seq: 1,
            kind: PacketKind::Data,
            payload: b"hi".to_vec(),
        };
        let mut input = vec![0x01, 0x02, 0x03, 0xFF, 0xAA];
        input.extend_from_slice(&encode_packet(&pkt));

        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&input), vec![pkt]);
    }

    /// The junk here is itself a bogus, incomplete frame - a stray sync
    /// byte followed by a length that promises more bytes than actually
    /// follow before the real packet's own sync byte arrives. The real
    /// sync byte must abandon the bogus frame outright rather than
    /// somehow stitching the two together.
    #[test]
    fn resynchronises_after_a_dropped_partial_frame() {
        let pkt = Packet {
            seq: 4,
            kind: PacketKind::Data,
            payload: b"yo".to_vec(),
        };
        let mut input = vec![0x7E, 0x05, 0x00, 0x02, 0x99, 0x99];
        input.extend_from_slice(&encode_packet(&pkt));

        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&input), vec![pkt]);
    }

    #[test]
    fn a_240_byte_payload_round_trips() {
        let payload: Vec<u8> = (0..MAX_PAYLOAD).map(|i| (i % 256) as u8).collect();
        let pkt = Packet {
            seq: 0,
            kind: PacketKind::Data,
            payload: payload.clone(),
        };
        let encoded = encode_packet(&pkt);
        let mut reader = PacketReader::new();
        assert_eq!(reader.push(&encoded), vec![pkt]);
    }

    #[test]
    #[should_panic(expected = "exceeds the 240-byte maximum")]
    fn a_241_byte_payload_is_rejected_at_encode() {
        let pkt = Packet {
            seq: 0,
            kind: PacketKind::Data,
            payload: vec![0u8; MAX_PAYLOAD + 1],
        };
        let _ = encode_packet(&pkt);
    }
}
