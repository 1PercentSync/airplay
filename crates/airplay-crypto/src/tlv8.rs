//! HomeKit TLV8 (one level; values >255 bytes are chunked).
//!
//! `[evidence: pyatv hap_tlv8.py:77-123; owntone pair-tlv.c:116-207]`

use std::collections::BTreeMap;

use airplay_core::{Error, Result};

/// HAP TLV type numbers.
///
/// `[evidence: pyatv hap_tlv8.py:13-34; owntone pair-tlv.h:9-32]`
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum TlvValue {
    Method = 0x00,
    Identifier = 0x01,
    Salt = 0x02,
    PublicKey = 0x03,
    Proof = 0x04,
    EncryptedData = 0x05,
    State = 0x06,
    Error = 0x07,
    BackOff = 0x08,
    Certificate = 0x09,
    Signature = 0x0A,
    Permissions = 0x0B,
    Flags = 0x13,
}

impl TlvValue {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x00 => Self::Method,
            0x01 => Self::Identifier,
            0x02 => Self::Salt,
            0x03 => Self::PublicKey,
            0x04 => Self::Proof,
            0x05 => Self::EncryptedData,
            0x06 => Self::State,
            0x07 => Self::Error,
            0x08 => Self::BackOff,
            0x09 => Self::Certificate,
            0x0A => Self::Signature,
            0x0B => Self::Permissions,
            0x13 => Self::Flags,
            _ => return None,
        })
    }
}

/// Insertion-ordered TLV map. Repeated tags are concatenated (chunk join).
#[derive(Clone, Debug, Default)]
pub struct TlvMap {
    items: Vec<(u8, Vec<u8>)>,
}

impl TlvMap {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn insert(&mut self, tag: TlvValue, value: impl Into<Vec<u8>>) {
        self.items.push((tag as u8, value.into()));
    }

    pub fn insert_u8(&mut self, tag: TlvValue, value: u8) {
        self.insert(tag, vec![value]);
    }

    pub fn get(&self, tag: TlvValue) -> Option<&[u8]> {
        let t = tag as u8;
        self.items
            .iter()
            .find(|(k, _)| *k == t)
            .map(|(_, v)| v.as_slice())
    }

    pub fn error_code(&self) -> Option<u8> {
        self.get(TlvValue::Error).and_then(|v| v.first().copied())
    }

    /// Encode, splitting any value longer than 255 bytes into consecutive chunks.
    ///
    /// `[evidence: pyatv hap_tlv8.py:109-123; owntone pair-tlv.c:144-152]`
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in &self.items {
            if value.is_empty() {
                out.push(*tag);
                out.push(0);
                continue;
            }
            let mut pos = 0;
            while pos < value.len() {
                let n = (value.len() - pos).min(255);
                out.push(*tag);
                out.push(n as u8);
                out.extend_from_slice(&value[pos..pos + n]);
                pos += n;
            }
        }
        out
    }

    /// Decode, joining consecutive same-tag chunks.
    ///
    /// `[evidence: pyatv hap_tlv8.py:84-100; owntone pair-tlv.c:161-206]`
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut joined: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
        let mut order: Vec<u8> = Vec::new();
        let mut i = 0;
        while i + 1 < data.len() {
            let tag = data[i];
            let len = data[i + 1] as usize;
            i += 2;
            if i + len > data.len() {
                return Err(Error::Protocol(format!(
                    "tlv truncated: tag {tag} needs {len} bytes"
                )));
            }
            if let Some(buf) = joined.get_mut(&tag) {
                buf.extend_from_slice(&data[i..i + len]);
            } else {
                order.push(tag);
                joined.insert(tag, data[i..i + len].to_vec());
            }
            i += len;
        }
        if i != data.len() {
            return Err(Error::Protocol("tlv trailing byte".into()));
        }
        let items = order
            .into_iter()
            .map(|tag| (tag, joined.remove(&tag).unwrap_or_default()))
            .collect();
        Ok(Self { items })
    }

    /// Short summary for logs (no secret material).
    pub fn summary(&self) -> String {
        self.items
            .iter()
            .map(|(tag, v)| {
                let name = TlvValue::from_u8(*tag)
                    .map(|t| format!("{t:?}"))
                    .unwrap_or_else(|| format!("0x{tag:02x}"));
                if *tag == TlvValue::Method as u8
                    || *tag == TlvValue::State as u8
                    || *tag == TlvValue::Flags as u8
                    || *tag == TlvValue::Error as u8
                {
                    let n = v.first().copied().unwrap_or(0);
                    format!("{name}={n}")
                } else {
                    format!("{name}={}B", v.len())
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small() {
        let mut m = TlvMap::new();
        m.insert_u8(TlvValue::State, 1);
        m.insert_u8(TlvValue::Method, 0);
        m.insert_u8(TlvValue::Flags, 0x10);
        let enc = m.encode();
        let dec = TlvMap::decode(&enc).unwrap();
        assert_eq!(dec.get(TlvValue::State), Some(&[1u8][..]));
        assert_eq!(dec.get(TlvValue::Method), Some(&[0u8][..]));
        assert_eq!(dec.get(TlvValue::Flags), Some(&[0x10u8][..]));
    }

    #[test]
    fn chunks_over_255() {
        let mut m = TlvMap::new();
        let pk = vec![0xABu8; 384];
        m.insert(TlvValue::PublicKey, pk.clone());
        let enc = m.encode();
        // 255 + 129 = 384, two chunks: 2+255 + 2+129 = 388
        assert_eq!(enc.len(), 388);
        assert_eq!(enc[0], TlvValue::PublicKey as u8);
        assert_eq!(enc[1], 255);
        assert_eq!(enc[257], TlvValue::PublicKey as u8);
        assert_eq!(enc[258], 129);
        let dec = TlvMap::decode(&enc).unwrap();
        assert_eq!(dec.get(TlvValue::PublicKey), Some(pk.as_slice()));
    }
}
