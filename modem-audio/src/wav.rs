//! Minimal WAV (RIFF/PCM) reader and writer.
//!
//! `modem-core` never touches a file - this crate is the one place that
//! turns samples into bytes on disk and back again. The two directions
//! `tests/interop.rs` actually needs are: write mono 16-bit PCM at
//! whatever rate `Tx` was configured for, so minimodem's own receiver can
//! read it back, and read back whatever minimodem itself wrote - measured
//! as 48 kHz mono 16-bit PCM, see that file's own probe. Other PCM widths
//! and channel counts decode correctly where the format makes an
//! unambiguous reading possible (Task 13's cpal backend may hand this
//! reader files from other sources), but only the 16-bit mono path this
//! crate's own tests exercise is load-bearing.

use std::io::{Read, Write};

/// Writes `samples` as a mono 16-bit PCM WAV at `rate` Hz.
///
/// Samples are expected in roughly `[-1.0, 1.0]`, matching every other
/// buffer in this workspace; values outside that range are clamped rather
/// than wrapped; a wrapped sample would misrepresent the signal far worse
/// than a clipped one does.
pub fn write_wav<W: Write>(mut w: W, samples: &[f32], rate: u32) {
    const BITS_PER_SAMPLE: u16 = 16;
    const CHANNELS: u16 = 1;

    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);
    let byte_rate = rate * u32::from(block_align);
    let data_len = samples.len() as u32 * u32::from(block_align);
    // Everything after the initial "RIFF" tag and this length field:
    // "WAVE" (4) + fmt chunk (8 header + 16 body) + data chunk (8 header
    // + data_len).
    let riff_len = 4 + 8 + 16 + 8 + data_len;

    w.write_all(b"RIFF").expect("write RIFF tag");
    w.write_all(&riff_len.to_le_bytes())
        .expect("write RIFF length");
    w.write_all(b"WAVE").expect("write WAVE tag");

    w.write_all(b"fmt ").expect("write fmt tag");
    w.write_all(&16u32.to_le_bytes()).expect("write fmt length");
    w.write_all(&1u16.to_le_bytes())
        .expect("write PCM format tag");
    w.write_all(&CHANNELS.to_le_bytes())
        .expect("write channel count");
    w.write_all(&rate.to_le_bytes()).expect("write sample rate");
    w.write_all(&byte_rate.to_le_bytes())
        .expect("write byte rate");
    w.write_all(&block_align.to_le_bytes())
        .expect("write block align");
    w.write_all(&BITS_PER_SAMPLE.to_le_bytes())
        .expect("write bits per sample");

    w.write_all(b"data").expect("write data tag");
    w.write_all(&data_len.to_le_bytes())
        .expect("write data length");
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let v = (clamped * 32767.0).round() as i16;
        w.write_all(&v.to_le_bytes()).expect("write sample");
    }
}

/// Reads a PCM WAV file into samples scaled to `[-1.0, 1.0]` and its
/// sample rate. Multi-channel input is downmixed to mono by averaging,
/// since everything downstream of this crate - `modem-core`'s `Rx` -
/// takes a single channel.
///
/// Panics on anything that is not a well-formed PCM WAV. There is no
/// recovery available from a corrupt or foreign file here, so a Result
/// this crate's only two callers would immediately `.expect()` on anyway
/// adds a layer without adding a choice.
pub fn read_wav<R: Read>(mut r: R) -> (Vec<f32>, u32) {
    let mut tag = [0u8; 4];
    r.read_exact(&mut tag).expect("read RIFF tag");
    assert_eq!(&tag, b"RIFF", "not a RIFF file");
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).expect("read RIFF length");
    r.read_exact(&mut tag).expect("read WAVE tag");
    assert_eq!(&tag, b"WAVE", "not a WAVE file");

    let mut channels: u16 = 1;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u16 = 16;
    let mut audio_format: u16 = 1;
    let mut samples: Vec<f32> = Vec::new();
    let mut have_fmt = false;
    let mut have_data = false;

    loop {
        let mut id = [0u8; 4];
        if r.read_exact(&mut id).is_err() {
            break; // end of stream, no trailing chunk
        }
        let mut size_buf = [0u8; 4];
        r.read_exact(&mut size_buf).expect("read chunk size");
        let size = u32::from_le_bytes(size_buf) as usize;

        match &id {
            b"fmt " => {
                let mut body = vec![0u8; size];
                r.read_exact(&mut body).expect("read fmt chunk");
                assert!(body.len() >= 16, "fmt chunk shorter than a PCM fmt chunk");
                audio_format = u16::from_le_bytes([body[0], body[1]]);
                channels = u16::from_le_bytes([body[2], body[3]]);
                sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                bits_per_sample = u16::from_le_bytes([body[14], body[15]]);
                have_fmt = true;
            }
            b"data" => {
                assert!(have_fmt, "data chunk arrived before fmt chunk");
                let mut body = vec![0u8; size];
                r.read_exact(&mut body).expect("read data chunk");
                samples = decode_pcm(&body, channels, bits_per_sample, audio_format);
                have_data = true;
            }
            _ => {
                let mut discard = vec![0u8; size];
                r.read_exact(&mut discard).expect("skip unknown chunk");
            }
        }

        if size % 2 == 1 {
            // RIFF pads every chunk to an even length; the pad byte is
            // not counted in the chunk's own size field. Not present
            // after the very last chunk in some encoders' output, so a
            // failed read here is not itself an error.
            let mut pad = [0u8; 1];
            let _ = r.read_exact(&mut pad);
        }
    }

    assert!(have_fmt, "WAV had no fmt chunk");
    assert!(have_data, "WAV had no data chunk");
    (samples, sample_rate)
}

/// Decodes one chunk's worth of interleaved PCM frames to mono `f32`.
fn decode_pcm(body: &[u8], channels: u16, bits_per_sample: u16, audio_format: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(body.len() / frame_bytes);
    for frame in body.chunks_exact(frame_bytes) {
        let mut acc = 0.0f32;
        for ch in frame.chunks_exact(bytes_per_sample) {
            acc += decode_one(ch, bits_per_sample, audio_format);
        }
        out.push(acc / channels as f32);
    }
    out
}

/// Decodes one sample of `bits_per_sample` width to `[-1.0, 1.0]`.
/// `audio_format` 1 is integer PCM, 3 is IEEE float - the only two WAV
/// actually defines.
fn decode_one(bytes: &[u8], bits_per_sample: u16, audio_format: u16) -> f32 {
    match (bits_per_sample, audio_format) {
        // 8-bit PCM is the one width WAV stores unsigned rather than
        // signed, a quirk of the original IBM format this crate has to
        // honour rather than one it chose.
        (8, _) => (bytes[0] as f32 - 128.0) / 128.0,
        (16, _) => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32768.0,
        (24, _) => {
            let sign_extend = if bytes[2] & 0x80 != 0 { 0xFFu8 } else { 0x00 };
            let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], sign_extend]);
            v as f32 / 8_388_608.0
        }
        (32, 3) => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        (32, _) => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0
        }
        _ => panic!("unsupported PCM format: {bits_per_sample}-bit, format tag {audio_format}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_mono_16_bit() {
        let samples: Vec<f32> = (0..800)
            .map(|i| (i as f32 / 800.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let mut buf = Vec::new();
        write_wav(&mut buf, &samples, 8000);
        let (got, rate) = read_wav(Cursor::new(buf));
        assert_eq!(rate, 8000);
        assert_eq!(got.len(), samples.len());
        for (a, b) in got.iter().zip(&samples) {
            assert!(
                (a - b).abs() < 1e-4,
                "round trip drifted: got {a}, want {b}"
            );
        }
    }

    /// A round trip through 16-bit quantisation is not lossless, so this
    /// pins the actual header fields and byte layout rather than trusting
    /// a value comparison to catch a swapped channel count or byte order.
    #[test]
    fn header_fields_are_correct() {
        let mut buf = Vec::new();
        write_wav(&mut buf, &[0.0f32; 4], 48000);
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[12..16], b"fmt ");
        assert_eq!(u16::from_le_bytes([buf[20], buf[21]]), 1, "not PCM");
        assert_eq!(u16::from_le_bytes([buf[22], buf[23]]), 1, "not mono");
        assert_eq!(
            u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
            48000,
            "wrong sample rate in header"
        );
        assert_eq!(
            u16::from_le_bytes([buf[34], buf[35]]),
            16,
            "not 16 bits per sample"
        );
        assert_eq!(&buf[36..40], b"data");
    }

    /// A sample outside [-1.0, 1.0] must clamp to a full-scale value, not
    /// wrap around into the opposite sign - wrapping would turn a loud
    /// clip into a phase-inverted signal, a much bigger lie.
    #[test]
    fn out_of_range_samples_clamp_rather_than_wrap() {
        let mut buf = Vec::new();
        write_wav(&mut buf, &[2.0f32, -2.0], 8000);
        let (got, _) = read_wav(Cursor::new(buf));
        assert!(
            got[0] > 0.99,
            "positive overshoot did not clamp: {}",
            got[0]
        );
        assert!(
            got[1] < -0.99,
            "negative overshoot did not clamp: {}",
            got[1]
        );
    }

    /// Two channels averaged, not one discarded - a reader that just took
    /// the left channel would still pass a test built from identical
    /// stereo content, so this uses channels that disagree.
    #[test]
    fn stereo_input_downmixes_by_averaging() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes()); // length, unchecked by the reader
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&2u16.to_le_bytes()); // stereo
        buf.extend_from_slice(&8000u32.to_le_bytes());
        buf.extend_from_slice(&32000u32.to_le_bytes()); // byte rate, unchecked
        buf.extend_from_slice(&4u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(b"data");
        let left = 16384i16;
        let right = -16384i16;
        let frame_bytes = [left.to_le_bytes(), right.to_le_bytes()].concat();
        buf.extend_from_slice(&(frame_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&frame_bytes);

        let (got, rate) = read_wav(Cursor::new(buf));
        assert_eq!(rate, 8000);
        assert_eq!(got.len(), 1);
        assert!(
            got[0].abs() < 1e-3,
            "opposite-sign channels did not average to near zero: {}",
            got[0]
        );
    }

    /// An unknown chunk (here a fake 'LIST') ahead of 'data' must be
    /// skipped, not misread as sample data or treated as a fatal error -
    /// real-world encoders routinely add metadata chunks minimodem itself
    /// does not emit but another tool might.
    #[test]
    fn unknown_chunks_before_data_are_skipped() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&8000u32.to_le_bytes());
        buf.extend_from_slice(&16000u32.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        // An odd-length unknown chunk, so the padding-byte path runs too.
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&[0x41u8, 0x42, 0x43]);
        buf.push(0); // pad byte
        buf.extend_from_slice(b"data");
        let sample = 1000i16.to_le_bytes();
        buf.extend_from_slice(&(sample.len() as u32).to_le_bytes());
        buf.extend_from_slice(&sample);

        let (got, rate) = read_wav(Cursor::new(buf));
        assert_eq!(rate, 8000);
        assert_eq!(got.len(), 1);
        assert!((got[0] - 1000.0 / 32768.0).abs() < 1e-6);
    }

    #[test]
    fn decodes_8_bit_pcm() {
        // 8-bit PCM is stored unsigned; 255 is full-scale positive, 0 is
        // full-scale negative, 128 is the zero point.
        assert!((decode_one(&[255], 8, 1) - 0.9921875).abs() < 1e-6);
        assert!((decode_one(&[0], 8, 1) - -1.0).abs() < 1e-6);
        assert!(decode_one(&[128], 8, 1).abs() < 1e-6);
    }

    #[test]
    fn decodes_32_bit_float_pcm() {
        let bytes = 0.25f32.to_le_bytes();
        assert!((decode_one(&bytes, 32, 3) - 0.25).abs() < 1e-6);
    }
}
