//! Apple binary property list (bplist00), minimal subset.
//!
//! Types: null, bool, int, real, date, data, ASCII/UTF-16 string, array, dict.
//! `[evidence: airplay2-sender-cpp airplay_crypto.cpp:450-751 (C-level code layout); pyatv uses plistlib]`

use std::collections::BTreeMap;
use std::fmt;

use airplay_core::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Real(f64),
    Date(f64),
    Data(Vec<u8>),
    String(String),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn dict_get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i128> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt_value(self, f, 0)
    }
}

/// Decimal plus hex of the value's two's-complement bits (64-bit if it fits).
/// Avoids i128 `{:x}` sign-extending an 8-byte plist int to 128 bits.
fn fmt_int(n: i128) -> String {
    if (0..=255).contains(&n) {
        format!("{n}")
    } else if n >= i64::MIN as i128 && n <= i64::MAX as i128 {
        format!("{n} (0x{:x})", n as u64)
    } else {
        format!("{n} (0x{:x})", n as u128)
    }
}

fn fmt_value(v: &Value, f: &mut fmt::Formatter<'_>, indent: usize) -> fmt::Result {
    let pad = "  ".repeat(indent);
    match v {
        Value::Null => write!(f, "null"),
        Value::Bool(b) => write!(f, "{b}"),
        Value::Int(n) => write!(f, "{}", fmt_int(*n)),
        Value::Real(r) => write!(f, "{r}"),
        Value::Date(d) => write!(f, "date({d})"),
        Value::Data(d) => write!(f, "data({} bytes)", d.len()),
        Value::String(s) => write!(f, "{s:?}"),
        Value::Array(arr) => {
            writeln!(f, "[")?;
            for item in arr {
                write!(f, "{pad}  ")?;
                fmt_value(item, f, indent + 1)?;
                writeln!(f)?;
            }
            write!(f, "{pad}]")
        }
        Value::Dict(items) => {
            writeln!(f, "{{")?;
            for (k, val) in items {
                write!(f, "{pad}  {k}: ")?;
                fmt_value(val, f, indent + 1)?;
                writeln!(f)?;
            }
            write!(f, "{pad}}}")
        }
    }
}

pub fn decode(data: &[u8]) -> Result<Value> {
    if data.len() < 40 || &data[..8] != b"bplist00" {
        return Err(Error::Plist("not a bplist00".into()));
    }
    let tr = data.len() - 32;
    let offset_size = data[tr + 6] as usize;
    let ref_size = data[tr + 7] as usize;
    let num_objects = read_be(data, tr + 8, 8)?;
    let top_object = read_be(data, tr + 16, 8)?;
    let offset_table = read_be(data, tr + 24, 8)? as usize;
    let valid = |s: usize| matches!(s, 1 | 2 | 4 | 8);
    if !valid(offset_size) || !valid(ref_size) {
        return Err(Error::Plist("bad size field in trailer".into()));
    }
    if offset_table > data.len() || num_objects > data.len() as u64 {
        return Err(Error::Plist("trailer out of range".into()));
    }
    if num_objects > ((data.len() - offset_table) / offset_size) as u64 {
        return Err(Error::Plist("object count exceeds offset table".into()));
    }
    let mut offsets = Vec::with_capacity(num_objects as usize);
    for i in 0..num_objects {
        let at = offset_table + (i as usize) * offset_size;
        offsets.push(read_be(data, at, offset_size)? as usize);
    }
    let mut ctx = DecodeCtx {
        data,
        ref_size,
        offsets,
    };
    ctx.object(top_object, 0)
}

struct DecodeCtx<'a> {
    data: &'a [u8],
    ref_size: usize,
    offsets: Vec<usize>,
}

impl<'a> DecodeCtx<'a> {
    fn object(&mut self, idx: u64, depth: usize) -> Result<Value> {
        if depth > 32 {
            return Err(Error::Plist("plist nesting too deep".into()));
        }
        let i = idx as usize;
        if i >= self.offsets.len() {
            return Err(Error::Plist("object ref out of range".into()));
        }
        let mut pos = self.offsets[i];
        if pos >= self.data.len() {
            return Err(Error::Plist("object offset out of range".into()));
        }
        let marker = self.data[pos];
        pos += 1;
        let hi = marker & 0xF0;
        let lo = marker & 0x0F;

        match hi {
            0x00 => match marker {
                0x00 => Ok(Value::Null),
                0x08 => Ok(Value::Bool(false)),
                0x09 => Ok(Value::Bool(true)),
                _ => Ok(Value::Null),
            },
            0x10 => {
                let n = 1usize << lo;
                let raw = read_be(self.data, pos, n)?;
                let v = if n <= 8 {
                    // signed interpretation for 1/2/4/8-byte ints
                    let shift = 128 - n * 8;
                    ((raw as i128) << shift) >> shift
                } else {
                    raw as i128
                };
                Ok(Value::Int(v))
            }
            0x20 => {
                let n = 1usize << lo;
                let bits = read_be(self.data, pos, n)?;
                let r = if n == 8 {
                    f64::from_bits(bits)
                } else if n == 4 {
                    f32::from_bits(bits as u32) as f64
                } else {
                    return Err(Error::Plist("unsupported real width".into()));
                };
                Ok(Value::Real(r))
            }
            0x30 => {
                let n = 1usize << lo;
                let bits = read_be(self.data, pos, n)?;
                Ok(Value::Date(f64::from_bits(bits)))
            }
            0x40 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                self.slice(pos, cnt).map(|b| Value::Data(b.to_vec()))
            }
            0x50 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let bytes = self.slice(pos, cnt)?;
                Ok(Value::String(String::from_utf8_lossy(bytes).into_owned()))
            }
            0x60 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let bytes = self.slice(pos, cnt.saturating_mul(2))?;
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                Ok(Value::String(String::from_utf16_lossy(&units)))
            }
            0xA0 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let mut arr = Vec::with_capacity(cnt as usize);
                for j in 0..cnt {
                    let r = read_be(self.data, pos + (j as usize) * self.ref_size, self.ref_size)?;
                    arr.push(self.object(r, depth + 1)?);
                }
                Ok(Value::Array(arr))
            }
            0xD0 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let mut dict = Vec::with_capacity(cnt as usize);
                for j in 0..cnt {
                    let kr = read_be(self.data, pos + (j as usize) * self.ref_size, self.ref_size)?;
                    let vr = read_be(
                        self.data,
                        pos + (cnt as usize + j as usize) * self.ref_size,
                        self.ref_size,
                    )?;
                    let k = match self.object(kr, depth + 1)? {
                        Value::String(s) => s,
                        other => format!("{other}"),
                    };
                    dict.push((k, self.object(vr, depth + 1)?));
                }
                Ok(Value::Dict(dict))
            }
            _ => Err(Error::Plist(format!("unsupported marker 0x{marker:02x}"))),
        }
    }

    fn read_count(&self, lo: u8, mut pos: usize) -> Result<(u64, usize)> {
        if lo != 0x0F {
            return Ok((lo as u64, pos));
        }
        if pos >= self.data.len() {
            return Err(Error::Plist("truncated count".into()));
        }
        let im = self.data[pos];
        pos += 1;
        let n = 1usize << (im & 0x0F);
        let c = read_be(self.data, pos, n)?;
        Ok((c, pos + n))
    }

    fn slice(&self, pos: usize, len: u64) -> Result<&'a [u8]> {
        let end = pos
            .checked_add(len as usize)
            .ok_or_else(|| Error::Plist("overflow".into()))?;
        if end > self.data.len() {
            return Err(Error::Plist("truncated payload".into()));
        }
        Ok(&self.data[pos..end])
    }
}

fn read_be(data: &[u8], at: usize, n: usize) -> Result<u64> {
    if n == 0 || n > 8 || at + n > data.len() {
        return Err(Error::Plist("read out of range".into()));
    }
    let mut v = 0u64;
    for i in 0..n {
        v = (v << 8) | data[at + i] as u64;
    }
    Ok(v)
}

/// Encode a value as bplist00. Root is object 0; 4-byte refs/offsets.
pub fn encode(root: &Value) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    fn add(objects: &mut Vec<Vec<u8>>, v: &Value) -> u32 {
        let idx = objects.len();
        objects.push(Vec::new());
        let encoded = encode_one(objects, v);
        objects[idx] = encoded;
        idx as u32
    }
    fn encode_one(objects: &mut Vec<Vec<u8>>, v: &Value) -> Vec<u8> {
        let mut obj = Vec::new();
        match v {
            Value::Null => obj.push(0x00),
            Value::Bool(false) => obj.push(0x08),
            Value::Bool(true) => obj.push(0x09),
            Value::Int(n) => encode_int(&mut obj, *n),
            Value::Real(r) => {
                obj.push(0x23);
                obj.extend_from_slice(&r.to_bits().to_be_bytes());
            }
            Value::Date(d) => {
                obj.push(0x33);
                obj.extend_from_slice(&d.to_bits().to_be_bytes());
            }
            Value::Data(d) => {
                encode_marker_len(&mut obj, 0x40, d.len() as u64);
                obj.extend_from_slice(d);
            }
            Value::String(s) => {
                if s.is_ascii() {
                    encode_marker_len(&mut obj, 0x50, s.len() as u64);
                    obj.extend_from_slice(s.as_bytes());
                } else {
                    let u16s: Vec<u16> = s.encode_utf16().collect();
                    encode_marker_len(&mut obj, 0x60, u16s.len() as u64);
                    for c in u16s {
                        obj.extend_from_slice(&c.to_be_bytes());
                    }
                }
            }
            Value::Array(arr) => {
                let refs: Vec<u32> = arr.iter().map(|c| add(objects, c)).collect();
                encode_marker_len(&mut obj, 0xA0, refs.len() as u64);
                for r in refs {
                    obj.extend_from_slice(&r.to_be_bytes());
                }
            }
            Value::Dict(items) => {
                let mut krefs = Vec::new();
                let mut vrefs = Vec::new();
                for (k, val) in items {
                    krefs.push(add(objects, &Value::String(k.clone())));
                    vrefs.push(add(objects, val));
                }
                encode_marker_len(&mut obj, 0xD0, items.len() as u64);
                for r in krefs {
                    obj.extend_from_slice(&r.to_be_bytes());
                }
                for r in vrefs {
                    obj.extend_from_slice(&r.to_be_bytes());
                }
            }
        }
        obj
    }
    add(&mut objects, root);

    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for o in &objects {
        offsets.push(out.len() as u64);
        out.extend_from_slice(o);
    }
    let offset_table = out.len() as u64;
    for off in &offsets {
        out.extend_from_slice(&(*off as u32).to_be_bytes());
    }
    let mut trailer = [0u8; 32];
    trailer[6] = 4;
    trailer[7] = 4;
    trailer[8..16].copy_from_slice(&(objects.len() as u64).to_be_bytes());
    trailer[16..24].copy_from_slice(&0u64.to_be_bytes());
    trailer[24..32].copy_from_slice(&offset_table.to_be_bytes());
    out.extend_from_slice(&trailer);
    out
}

fn encode_marker_len(obj: &mut Vec<u8>, marker: u8, len: u64) {
    if len < 15 {
        obj.push(marker | (len as u8));
    } else {
        obj.push(marker | 0x0F);
        encode_int(obj, len as i128);
    }
}

fn encode_int(obj: &mut Vec<u8>, value: i128) {
    if (0..=0xFF).contains(&value) {
        obj.push(0x10);
        obj.push(value as u8);
    } else if (0..=0xFFFF).contains(&value) {
        obj.push(0x11);
        obj.extend_from_slice(&(value as u16).to_be_bytes());
    } else if (0..=0xFFFF_FFFF).contains(&value) {
        obj.push(0x12);
        obj.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        obj.push(0x13);
        obj.extend_from_slice(&(value as u64).to_be_bytes());
    }
}

/// Highlighted /info fields used by later SETUP.
pub fn info_highlights(root: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let pick = |k: &str| {
        root.dict_get(k).map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Int(n) => fmt_int(*n),
            Value::Bool(b) => b.to_string(),
            Value::Real(r) => r.to_string(),
            Value::Data(d) => format!("data({} bytes)", d.len()),
            other => other.to_string().replace('\n', " "),
        })
    };
    for k in [
        "model",
        "name",
        "sourceVersion",
        "protocolVersion",
        "features",
        "statusFlags",
        "keepAliveSendStatsAsBody",
        "initialVolume",
        "volumeControlType",
        "deviceID",
        "macAddress",
        "senderAddress",
    ] {
        if let Some(v) = pick(k) {
            out.insert(k.to_string(), v);
        }
    }
    if let Some(sf) = root.dict_get("supportedFormats") {
        out.insert("supportedFormats".into(), sf.to_string().replace('\n', " "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_dict() {
        let v = Value::Dict(vec![
            ("name".into(), Value::String("HomePod".into())),
            ("features".into(), Value::Int(0x3C354BD04A7FCA00)),
            ("ok".into(), Value::Bool(true)),
            ("rate".into(), Value::Real(44100.0)),
            ("blob".into(), Value::Data(vec![1, 2, 3])),
            (
                "arr".into(),
                Value::Array(vec![Value::Int(1), Value::Int(2)]),
            ),
        ]);
        let bytes = encode(&v);
        assert!(bytes.starts_with(b"bplist00"));
        let back = decode(&bytes).unwrap();
        assert_eq!(
            back.dict_get("name").and_then(|x| x.as_str()),
            Some("HomePod")
        );
        assert_eq!(
            back.dict_get("features").and_then(|x| x.as_int()),
            Some(0x3C354BD04A7FCA00)
        );
        assert_eq!(back.dict_get("ok"), Some(&Value::Bool(true)));
    }

    #[test]
    fn int_hex_is_bit_pattern_not_i128_sign_extend() {
        let n: i128 = -577021992844656640;
        assert_eq!(fmt_int(n), "-577021992844656640 (0xf7fe018e00e80000)");
        assert_eq!(
            fmt_int(0x3C354BD04A7FCA00),
            "4338457174016510464 (0x3c354bd04a7fca00)"
        );
        assert_eq!(fmt_int(3), "3");
    }
}
