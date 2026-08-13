//! Uncompressed ALAC bit writer for 16-bit stereo.
//!
//! Field order matches shairport-sync `alac.c` decode and raop_sender encode.
//! [evidence: shairport-sync alac.c:653,816-839; raop_sender.cpp:1814-1835]

pub fn encode_alac_frame(frames: &[i16]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put(1, 3);
    w.put(0, 4);
    w.put(0, 12);
    w.put(0, 1);
    w.put(0, 2);
    w.put(1, 1);
    for s in frames {
        w.put(*s as u16 as u32, 16);
    }
    w.put(7, 3);
    w.finish()
}

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

    fn put(&mut self, value: u32, bits: u32) {
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

#[cfg(test)]
mod tests {
    use super::encode_alac_frame;

    #[test]
    fn two_silent_stereo_frames() {
        let got = encode_alac_frame(&[0, 0, 0, 0]);
        assert_eq!(
            got,
            vec![0x20, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xc0]
        );
    }

    #[test]
    fn packet_of_352_silent_frames_length() {
        let samples = vec![0i16; 352 * 2];
        let got = encode_alac_frame(&samples);
        assert_eq!(got.len(), 1412);
    }
}
