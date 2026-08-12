//! TLV8 (Type-Length-Value, 8-bit type & length) used by HAP pairing.
//!
//! Values longer than 255 bytes are split into consecutive entries with the
//! same type (HAP continuation rule); decoding merges them back.
//!
//! [evidence: airplay-cli/src/ap2_hap.c:55-63 (TLV type constants)]

/// TLV type constants used in pair-setup / pair-verify.
pub mod types {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const SIGNATURE: u8 = 0x0A;
    pub const FLAGS: u8 = 0x13;
}

#[derive(Debug, thiserror::Error)]
pub enum TlvError {
    #[error("truncated TLV entry at offset {0}")]
    Truncated(usize),
}

/// A decoded TLV map: ordered list of (type, value) with continuations merged.
pub type TlvMap = Vec<(u8, Vec<u8>)>;

/// Encode entries; any value > 255 bytes is emitted as continuation entries.
pub fn encode(entries: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, v) in entries {
        let mut rest = *v;
        loop {
            let n = rest.len().min(255);
            out.push(*t);
            out.push(n as u8);
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if rest.is_empty() {
                break;
            }
        }
    }
    out
}

/// Decode a TLV8 buffer, merging consecutive same-type entries.
pub fn decode(data: &[u8]) -> Result<TlvMap, TlvError> {
    let mut out: TlvMap = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= data.len() {
        let t = data[pos];
        let n = data[pos + 1] as usize;
        if pos + 2 + n > data.len() {
            return Err(TlvError::Truncated(pos));
        }
        let v = &data[pos + 2..pos + 2 + n];
        if let Some(last) = out.last_mut() {
            if last.0 == t {
                last.1.extend_from_slice(v);
                pos += 2 + n;
                continue;
            }
        }
        out.push((t, v.to_vec()));
        pos += 2 + n;
    }
    if pos != data.len() {
        return Err(TlvError::Truncated(pos));
    }
    Ok(out)
}

/// First value of a given type, if present.
pub fn get(map: &TlvMap, t: u8) -> Option<&[u8]> {
    map.iter().find(|(tt, _)| *tt == t).map(|(_, v)| v.as_slice())
}

/// Convenience: single-byte value (State / Error / Method).
pub fn get_u8(map: &TlvMap, t: u8) -> Option<u8> {
    get(map, t).and_then(|v| v.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::*;

    #[test]
    fn encodes_m1_transient_exactly() {
        // M1 = {Method: 0, State: 1, Flags: 0x10}
        // [evidence: airplay2-sender-cpp/src/raop_sender.cpp:1315-1331]
        let m = encode(&[(METHOD, &[0x00]), (STATE, &[0x01]), (FLAGS, &[0x10])]);
        assert_eq!(m, vec![0x00, 0x01, 0x00, 0x06, 0x01, 0x01, 0x13, 0x01, 0x10]);
    }

    #[test]
    fn roundtrip_with_continuation() {
        let big = vec![0xABu8; 384]; // SRP public key size forces continuation
        let m = encode(&[(PUBLIC_KEY, &big)]);
        assert_eq!(m[1], 255);
        let d = decode(&m).unwrap();
        assert_eq!(get(&d, PUBLIC_KEY).unwrap(), &big[..]);
    }

    #[test]
    fn decode_rejects_truncation() {
        assert!(decode(&[0x03, 0x05, 0x01]).is_err());
    }
}
