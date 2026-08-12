//! Uncompressed-escape ALAC frame writer (16-bit, stereo, spf=352).
//!
//! The realtime stream (type 0x60) is hardcoded to ALAC on receivers; we emit
//! the uncompressed escape frame: header bits + raw MSB-first samples + END.
//!
//! [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1814-1837;
//!  cross-check: shairport-sync alac.c decode]

/// Bit writer (MSB-first), matches the reference `put(value, bits)` exactly.
struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    filled: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            cur: 0,
            filled: 0,
        }
    }

    fn put(&mut self, value: u32, bits: u8) {
        for i in (0..bits).rev() {
            self.cur = (self.cur << 1) | (((value >> i) & 1) as u8);
            self.filled += 1;
            if self.filled == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.filled = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.cur <<= 8 - self.filled;
            self.out.push(self.cur);
        }
        self.out
    }
}

/// Encode `frames * channels` interleaved 16-bit samples as one ALAC frame.
/// `samples` must be interleaved L,R,L,R… (channels = 2).
pub fn encode_frame(samples: &[i16], frames: usize) -> Vec<u8> {
    debug_assert_eq!(samples.len(), frames * 2);
    let mut w = BitWriter::new();
    w.put(1, 3); // stereo channel-pair element
    w.put(0, 4); // unused
    w.put(0, 12); // unknown
    w.put(0, 1); // hasSize = 0 → default frame length from the cookie
    w.put(0, 2); // wastedBytes = 0
    w.put(1, 1); // isNotCompressed = 1 (uncompressed escape)
    for &s in samples {
        w.put(s as u16 as u32, 16);
    }
    w.put(7, 3); // END element tag
    w.finish()
}

/// One full ALAC frame of digital silence (spf samples).
pub fn silence_frame(spf: usize) -> Vec<u8> {
    encode_frame(&vec![0i16; spf * 2], spf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use airplay_core::FRAMES_PER_PACKET;

    /// Independent re-implementation: build the expected bit string by hand
    /// and convert to bytes. Disagreement = a bug in BitWriter or layout.
    fn expected_bits(samples: &[i16]) -> Vec<u8> {
        let mut bits = String::new();
        bits.push_str("001"); // CPE
        bits.push_str("0000");
        bits.push_str("000000000000");
        bits.push('0');
        bits.push_str("00");
        bits.push('1');
        for &s in samples {
            bits.push_str(&format!("{:016b}", s as u16));
        }
        bits.push_str("111");
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        (0..bits.len() / 8)
            .map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap())
            .collect()
    }

    #[test]
    fn matches_independent_bit_layout() {
        let samples: Vec<i16> = vec![0x1234, -1, 0x5678, 0x0F0F, i16::MIN, i16::MAX];
        assert_eq!(encode_frame(&samples, 3), expected_bits(&samples));
    }

    #[test]
    fn silence_352_golden() {
        let f = silence_frame(FRAMES_PER_PACKET);
        // 23 header bits + 352*2*16 sample bits + 3 END bits = 11290 → 1412 B.
        // Header bits: 001 0000 000000000000 0 00 1 → bytes [0x20, 0x00, 0x02].
        assert_eq!(f.len(), 1412);
        assert_eq!(&f[..3], &[0x20, 0x00, 0x02]);
        assert_eq!(f[3], 0x00); // first 8 bits of the first silent sample
        // END tag (111) at bits 11287..11289 → byte 1410 = 0x01, byte 1411 = 0xC0.
        assert!(f[4..1410].iter().all(|&b| b == 0));
        assert_eq!(f[1410], 0x01);
        assert_eq!(f[1411], 0xC0);
    }
}
