//! HAP TLV8: one-byte type, one-byte length (0–255), value; same-tag fragments join.
//!
//! Encode: empty value → one type+0 record; non-empty → 255-byte chunks, no extra
//! zero tail. Decode: concatenate **all** same tags (not only adjacent).
//!
//! [evidence: pyatv hap_tlv8.py:77-123; owntone pair-tlv.c:116-207;
//! raop_sender airplay_crypto.cpp:404-438; airplay2-receiver hap.py:74-128]

use airplay_core::{Error, Result};
use std::collections::HashMap;

pub type TlvMap = HashMap<u8, Vec<u8>>;

pub struct TlvType;

impl TlvType {
    pub const METHOD: u8 = 0;
    pub const SALT: u8 = 2;
    pub const PUBLIC_KEY: u8 = 3;
    pub const PROOF: u8 = 4;
    pub const STATE: u8 = 6;
    pub const ERROR: u8 = 7;
    pub const FLAGS: u8 = 0x13;
}

pub const FLAG_TRANSIENT: u8 = 0x10;

/// Encode `(tag, value)` pairs in the given order.
pub fn encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(tag, value) in items {
        if value.is_empty() {
            // [evidence: owntone pair-tlv.c:134-139; raop_sender encode do-while]
            out.push(tag);
            out.push(0);
            continue;
        }
        let mut pos = 0;
        while pos < value.len() {
            let n = (value.len() - pos).min(255);
            out.push(tag);
            out.push(n as u8);
            out.extend_from_slice(&value[pos..pos + n]);
            pos += n;
        }
    }
    out
}

pub fn decode(data: &[u8]) -> Result<TlvMap> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i < data.len() {
        if i + 2 > data.len() {
            return Err(Error::Tlv("truncated TLV header".into()));
        }
        let tag = data[i];
        let len = data[i + 1] as usize;
        if i + 2 + len > data.len() {
            return Err(Error::Tlv("truncated TLV value".into()));
        }
        let chunk = &data[i + 2..i + 2 + len];
        map.entry(tag).or_insert_with(Vec::new).extend_from_slice(chunk);
        i += 2 + len;
    }
    Ok(map)
}

pub fn get<'a>(map: &'a TlvMap, tag: u8) -> Option<&'a [u8]> {
    map.get(&tag).map(|v| v.as_slice())
}

pub fn get_u8(map: &TlvMap, tag: u8) -> Option<u8> {
    get(map, tag).and_then(|v| v.first().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_value_emits_type_and_zero() {
        let enc = encode(&[(TlvType::STATE, &[])]);
        assert_eq!(enc, vec![TlvType::STATE, 0]);
        let m = decode(&enc).unwrap();
        assert_eq!(get(&m, TlvType::STATE), Some(&[][..]));
    }

    #[test]
    fn split_255_and_roundtrip() {
        let v = vec![0xABu8; 256];
        let enc = encode(&[(TlvType::PUBLIC_KEY, &v)]);
        assert_eq!(enc[0], TlvType::PUBLIC_KEY);
        assert_eq!(enc[1], 255);
        assert_eq!(enc[257], TlvType::PUBLIC_KEY);
        assert_eq!(enc[258], 1);
        let m = decode(&enc).unwrap();
        assert_eq!(get(&m, TlvType::PUBLIC_KEY), Some(v.as_slice()));
    }

    #[test]
    fn exactly_255_no_zero_tail() {
        let v = vec![0x11u8; 255];
        let enc = encode(&[(TlvType::PUBLIC_KEY, &v)]);
        assert_eq!(enc.len(), 2 + 255);
        assert_eq!(enc[1], 255);
    }

    #[test]
    fn concat_nonadjacent_same_tag() {
        // pyatv concatenates all same tags, not only adjacent ones
        let bytes = encode(&[
            (TlvType::SALT, &[1, 2]),
            (TlvType::STATE, &[1]),
            (TlvType::SALT, &[3, 4]),
        ]);
        let m = decode(&bytes).unwrap();
        assert_eq!(get(&m, TlvType::SALT), Some(&[1, 2, 3, 4][..]));
    }

    #[test]
    fn public_key_384_splits_two_chunks() {
        let v = vec![0x03u8; 384];
        let enc = encode(&[(TlvType::PUBLIC_KEY, &v)]);
        assert_eq!(enc.len(), 2 + 255 + 2 + 129);
        let m = decode(&enc).unwrap();
        assert_eq!(m.get(&TlvType::PUBLIC_KEY).unwrap().len(), 384);
    }
}
