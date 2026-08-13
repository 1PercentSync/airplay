//! Binary plist aligned with CPython 3.12 `plistlib` (what pyatv uses).
//!
//! Integers: `tokenH==0x10`, width `1<<tokenL`; signed iff `tokenL>=3`;
//! `tokenL==4` reads 16 bytes. Encode `2^63..2^64-1` as `0x14` + 16 bytes.
//! Root = trailer `topObject`. Extended count: following int marker high
//! nibble must be `0x10`. UTF-8 decode is lossy.
//!
//! [evidence: research/references/cpython_3.12_plistlib_int.md;
//! pyatv support/rtsp.py:10,110,287-288;
//! raop_sender airplay_crypto.cpp:448-751 — C-level, do not copy 8-byte cap / root=0]

use airplay_core::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct PlistInt {
    /// 1, 2, 4, 8, or 16
    pub width: u8,
    /// Raw big-endian bits, not sign-extended past `width`.
    pub bits: u128,
}

impl PlistInt {
    pub fn from_i64(v: i64) -> Self {
        Self {
            width: 8,
            bits: v as u64 as u128,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        if self.width <= 8 {
            Some(self.bits as u64 as i64)
        } else {
            None
        }
    }

    /// Decimal plus, for 8-byte ints, 64-bit hex of the bit pattern.
    pub fn display_probe(&self) -> String {
        match self.width {
            8 => {
                let bits = self.bits as u64;
                format!("{} (0x{bits:016x})", bits as i64)
            }
            16 => {
                let signed = self.bits as i128;
                format!("{signed} (0x{:032x})", self.bits)
            }
            _ => format!("{}", self.bits),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(PlistInt),
    Real(f64),
    Date(f64),
    Uid(u64),
    String(String),
    Data(Vec<u8>),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn dict_get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
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

pub fn decode(data: &[u8]) -> Result<Value> {
    if data.len() < 8 + 32 || &data[..8] != b"bplist00" {
        return Err(Error::Plist("not bplist00".into()));
    }
    let tr = data.len() - 32;
    let offset_size = data[tr + 6] as usize;
    let ref_size = data[tr + 7] as usize;
    let num_objects = read_be(data, tr + 8, 8)? as usize;
    let top = read_be(data, tr + 16, 8)? as usize;
    let offset_table = read_be(data, tr + 24, 8)? as usize;
    if !matches!(offset_size, 1 | 2 | 4 | 8) || !matches!(ref_size, 1 | 2 | 4 | 8) {
        return Err(Error::Plist("bad trailer size fields".into()));
    }
    if num_objects == 0 || num_objects > data.len() {
        return Err(Error::Plist("bad numObjects".into()));
    }
    if offset_table > data.len()
        || num_objects > (data.len() - offset_table) / offset_size.max(1)
    {
        return Err(Error::Plist("offset table out of range".into()));
    }
    let mut offsets = Vec::with_capacity(num_objects);
    for i in 0..num_objects {
        offsets.push(read_be(data, offset_table + i * offset_size, offset_size)? as usize);
    }
    let mut ctx = Decoder {
        data,
        offsets: &offsets,
        ref_size,
    };
    ctx.object(top, 0)
}

struct Decoder<'a> {
    data: &'a [u8],
    offsets: &'a [usize],
    ref_size: usize,
}

impl<'a> Decoder<'a> {
    fn object(&mut self, idx: usize, depth: u32) -> Result<Value> {
        if depth > 64 {
            return Err(Error::Plist("too deep".into()));
        }
        if idx >= self.offsets.len() {
            return Err(Error::Plist("object ref out of range".into()));
        }
        let mut pos = self.offsets[idx];
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
                if n > 16 {
                    return Err(Error::Plist("int wider than 16 bytes".into()));
                }
                let raw = read_be_bytes(self.data, pos, n)?;
                Ok(Value::Int(PlistInt {
                    width: n as u8,
                    bits: raw,
                }))
            }
            0x20 => {
                let n = 1usize << lo;
                let bits = read_be(self.data, pos, n)?;
                let r = if n == 8 {
                    f64::from_bits(bits)
                } else if n == 4 {
                    f32::from_bits(bits as u32) as f64
                } else {
                    return Err(Error::Plist("bad real width".into()));
                };
                Ok(Value::Real(r))
            }
            0x30 => {
                if marker != 0x33 {
                    return Err(Error::Plist("unsupported date marker".into()));
                }
                let bits = read_be(self.data, pos, 8)?;
                Ok(Value::Date(f64::from_bits(bits)))
            }
            0x40 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let end = pos.checked_add(cnt).ok_or_else(|| Error::Plist("data overflow".into()))?;
                if end > self.data.len() {
                    return Err(Error::Plist("data truncated".into()));
                }
                Ok(Value::Data(self.data[pos..end].to_vec()))
            }
            0x50 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let end = pos.checked_add(cnt).ok_or_else(|| Error::Plist("ascii overflow".into()))?;
                if end > self.data.len() {
                    return Err(Error::Plist("ascii truncated".into()));
                }
                Ok(Value::String(String::from_utf8_lossy(&self.data[pos..end]).into_owned()))
            }
            0x60 => {
                let (cnt, pos) = self.read_count(lo, pos)?;
                let nbytes = cnt.checked_mul(2).ok_or_else(|| Error::Plist("utf16 overflow".into()))?;
                let end = pos.checked_add(nbytes).ok_or_else(|| Error::Plist("utf16 overflow".into()))?;
                if end > self.data.len() {
                    return Err(Error::Plist("utf16 truncated".into()));
                }
                let mut u16s = Vec::with_capacity(cnt);
                for i in 0..cnt {
                    let hi = self.data[pos + i * 2] as u16;
                    let lo = self.data[pos + i * 2 + 1] as u16;
                    u16s.push((hi << 8) | lo);
                }
                Ok(Value::String(String::from_utf16_lossy(&u16s)))
            }
            0x80 => {
                let n = (lo as usize) + 1;
                let bits = read_be(self.data, pos, n)?;
                Ok(Value::Uid(bits))
            }
            0xA0 => {
                let (cnt, mut pos) = self.read_count(lo, pos)?;
                let mut arr = Vec::with_capacity(cnt);
                for _ in 0..cnt {
                    let r = read_be(self.data, pos, self.ref_size)? as usize;
                    pos += self.ref_size;
                    arr.push(self.object(r, depth + 1)?);
                }
                Ok(Value::Array(arr))
            }
            0xD0 => {
                let (cnt, mut pos) = self.read_count(lo, pos)?;
                let mut keys = Vec::with_capacity(cnt);
                for _ in 0..cnt {
                    let r = read_be(self.data, pos, self.ref_size)? as usize;
                    pos += self.ref_size;
                    keys.push(self.object(r, depth + 1)?);
                }
                let mut pairs = Vec::with_capacity(cnt);
                for key in keys {
                    let r = read_be(self.data, pos, self.ref_size)? as usize;
                    pos += self.ref_size;
                    let k = match key {
                        Value::String(s) => s,
                        _ => return Err(Error::Plist("dict key not a string".into())),
                    };
                    pairs.push((k, self.object(r, depth + 1)?));
                }
                Ok(Value::Dict(pairs))
            }
            _ => Err(Error::Plist(format!("unsupported marker 0x{marker:02x}"))),
        }
    }

    fn read_count(&self, lo: u8, mut pos: usize) -> Result<(usize, usize)> {
        if lo != 0x0F {
            return Ok((lo as usize, pos));
        }
        if pos >= self.data.len() {
            return Err(Error::Plist("truncated extended count".into()));
        }
        let im = self.data[pos];
        pos += 1;
        if im & 0xF0 != 0x10 {
            return Err(Error::Plist("extended count is not an int object".into()));
        }
        let n = 1usize << (im & 0x0F);
        let c = read_be(self.data, pos, n)? as usize;
        Ok((c, pos + n))
    }
}

fn read_be(data: &[u8], at: usize, n: usize) -> Result<u64> {
    if n == 0 || n > 8 || at + n > data.len() {
        return Err(Error::Plist("read_be out of range".into()));
    }
    let mut v = 0u64;
    for i in 0..n {
        v = (v << 8) | data[at + i] as u64;
    }
    Ok(v)
}

fn read_be_bytes(data: &[u8], at: usize, n: usize) -> Result<u128> {
    if n == 0 || n > 16 || at + n > data.len() {
        return Err(Error::Plist("int bytes out of range".into()));
    }
    let mut v = 0u128;
    for i in 0..n {
        v = (v << 8) | data[at + i] as u128;
    }
    Ok(v)
}

/// Encode for unit-test round-trips. Integer widths follow plistlib.
pub fn encode(root: &Value) -> Result<Vec<u8>> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    let top = encode_add(&mut objects, root)?;
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for obj in &objects {
        offsets.push(out.len() as u64);
        out.extend_from_slice(obj);
    }
    let offset_table = out.len() as u64;
    let offset_size = min_int_size(out.len() as u64 + 32 + (objects.len() as u64) * 8);
    for off in offsets {
        append_be(&mut out, off, offset_size);
    }
    let ref_size = min_int_size((objects.len().saturating_sub(1)) as u64);
    let mut trailer = [0u8; 32];
    trailer[6] = offset_size as u8;
    trailer[7] = ref_size as u8;
    put_be64(&mut trailer, 8, objects.len() as u64);
    put_be64(&mut trailer, 16, top as u64);
    put_be64(&mut trailer, 24, offset_table);
    out.extend_from_slice(&trailer);
    if objects.len() > 255 {
        return encode_with_sizes(root, 2, offset_size.max(2));
    }
    Ok(out)
}

fn encode_with_sizes(root: &Value, ref_size: usize, offset_size: usize) -> Result<Vec<u8>> {
    let mut objects = Vec::new();
    let top = encode_add_sized(&mut objects, root, ref_size)?;
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for obj in &objects {
        offsets.push(out.len() as u64);
        out.extend_from_slice(obj);
    }
    let offset_table = out.len() as u64;
    for off in offsets {
        append_be(&mut out, off, offset_size);
    }
    let mut trailer = [0u8; 32];
    trailer[6] = offset_size as u8;
    trailer[7] = ref_size as u8;
    put_be64(&mut trailer, 8, objects.len() as u64);
    put_be64(&mut trailer, 16, top as u64);
    put_be64(&mut trailer, 24, offset_table);
    out.extend_from_slice(&trailer);
    Ok(out)
}

fn encode_add(objects: &mut Vec<Vec<u8>>, v: &Value) -> Result<usize> {
    encode_add_sized(objects, v, 1)
}

fn encode_add_sized(objects: &mut Vec<Vec<u8>>, v: &Value, ref_size: usize) -> Result<usize> {
    let idx = objects.len();
    objects.push(Vec::new());
    let encoded = encode_object(objects, v, ref_size)?;
    objects[idx] = encoded;
    Ok(idx)
}

fn encode_object(objects: &mut Vec<Vec<u8>>, v: &Value, ref_size: usize) -> Result<Vec<u8>> {
    let mut obj = Vec::new();
    match v {
        Value::Null => obj.push(0x00),
        Value::Bool(false) => obj.push(0x08),
        Value::Bool(true) => obj.push(0x09),
        Value::Int(i) => encode_int_stored(&mut obj, i),
        Value::Real(r) => {
            obj.push(0x23);
            obj.extend_from_slice(&r.to_bits().to_be_bytes());
        }
        Value::Date(d) => {
            obj.push(0x33);
            obj.extend_from_slice(&d.to_bits().to_be_bytes());
        }
        Value::Uid(u) => {
            let nbytes = min_int_size(*u).max(1);
            obj.push(0x80 | ((nbytes as u8) - 1));
            append_be(&mut obj, *u, nbytes);
        }
        Value::String(s) => {
            if s.bytes().all(|b| b < 128) {
                encode_marker_len(&mut obj, 0x50, s.len())?;
                obj.extend_from_slice(s.as_bytes());
            } else {
                let u16s: Vec<u16> = s.encode_utf16().collect();
                encode_marker_len(&mut obj, 0x60, u16s.len())?;
                for c in u16s {
                    obj.extend_from_slice(&c.to_be_bytes());
                }
            }
        }
        Value::Data(d) => {
            encode_marker_len(&mut obj, 0x40, d.len())?;
            obj.extend_from_slice(d);
        }
        Value::Array(arr) => {
            let mut refs = Vec::new();
            for c in arr {
                refs.push(encode_add_sized(objects, c, ref_size)?);
            }
            encode_marker_len(&mut obj, 0xA0, refs.len())?;
            for r in refs {
                append_be(&mut obj, r as u64, ref_size);
            }
        }
        Value::Dict(pairs) => {
            let mut krefs = Vec::new();
            let mut vrefs = Vec::new();
            for (k, val) in pairs {
                krefs.push(encode_add_sized(objects, &Value::String(k.clone()), ref_size)?);
                vrefs.push(encode_add_sized(objects, val, ref_size)?);
            }
            encode_marker_len(&mut obj, 0xD0, pairs.len())?;
            for r in krefs {
                append_be(&mut obj, r as u64, ref_size);
            }
            for r in vrefs {
                append_be(&mut obj, r as u64, ref_size);
            }
        }
    }
    Ok(obj)
}

fn encode_int_stored(obj: &mut Vec<u8>, i: &PlistInt) {
    match i.width {
        1 => {
            obj.push(0x10);
            obj.push(i.bits as u8);
        }
        2 => {
            obj.push(0x11);
            obj.extend_from_slice(&(i.bits as u16).to_be_bytes());
        }
        4 => {
            obj.push(0x12);
            obj.extend_from_slice(&(i.bits as u32).to_be_bytes());
        }
        8 => {
            obj.push(0x13);
            obj.extend_from_slice(&(i.bits as u64).to_be_bytes());
        }
        16 => {
            obj.push(0x14);
            obj.extend_from_slice(&i.bits.to_be_bytes());
        }
        _ => encode_int_plistlib(obj, i.bits as i128),
    }
}

fn encode_int_plistlib(obj: &mut Vec<u8>, value: i128) {
    if value < 0 {
        obj.push(0x13);
        obj.extend_from_slice(&(value as i64).to_be_bytes());
    } else if value < 1 << 8 {
        obj.push(0x10);
        obj.push(value as u8);
    } else if value < 1 << 16 {
        obj.push(0x11);
        obj.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value < 1 << 32 {
        obj.push(0x12);
        obj.extend_from_slice(&(value as u32).to_be_bytes());
    } else if value < 1 << 63 {
        obj.push(0x13);
        obj.extend_from_slice(&(value as u64).to_be_bytes());
    } else if value < (1i128 << 64) {
        obj.push(0x14);
        obj.extend_from_slice(&(value as u128).to_be_bytes());
    } else {
        obj.push(0x14);
        obj.extend_from_slice(&(value as u128).to_be_bytes());
    }
}

fn encode_marker_len(obj: &mut Vec<u8>, marker: u8, len: usize) -> Result<()> {
    if len < 15 {
        obj.push(marker | (len as u8));
    } else {
        obj.push(marker | 0x0F);
        encode_int_plistlib(obj, len as i128);
    }
    Ok(())
}

fn min_int_size(v: u64) -> usize {
    if v <= 0xFF {
        1
    } else if v <= 0xFFFF {
        2
    } else if v <= 0xFFFF_FFFF {
        4
    } else {
        8
    }
}

fn append_be(buf: &mut Vec<u8>, v: u64, n: usize) {
    for i in (0..n).rev() {
        buf.push(((v >> (8 * i)) & 0xFF) as u8);
    }
}

fn put_be64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

pub fn pretty_print(v: &Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.display_probe()),
        Value::Real(r) => out.push_str(&format!("{r}")),
        Value::Date(d) => out.push_str(&format!("date({d})")),
        Value::Uid(u) => out.push_str(&format!("uid({u})")),
        Value::String(s) => out.push_str(&format!("{s:?}")),
        Value::Data(d) => out.push_str(&format!("data({} bytes)", d.len())),
        Value::Array(a) => {
            out.push_str("[\n");
            for (i, item) in a.iter().enumerate() {
                out.push_str(&pad);
                out.push_str("  ");
                pretty_print(item, indent + 1, out);
                if i + 1 != a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push(']');
        }
        Value::Dict(pairs) => {
            out.push_str("{\n");
            for (i, (k, val)) in pairs.iter().enumerate() {
                out.push_str(&pad);
                out.push_str("  ");
                out.push_str(k);
                out.push_str(": ");
                pretty_print(val, indent + 1, out);
                if i + 1 != pairs.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_dict_and_eight_byte_int() {
        let bits: u64 = 0xf7fe_018e_00e8_0000;
        let v = Value::Dict(vec![
            ("name".into(), Value::String("HomePod".into())),
            (
                "bufferStream".into(),
                Value::Int(PlistInt {
                    width: 8,
                    bits: bits as u128,
                }),
            ),
            ("ok".into(), Value::Bool(true)),
            ("n".into(), Value::Int(PlistInt { width: 1, bits: 7 })),
        ]);
        let bytes = encode(&v).unwrap();
        assert!(bytes.starts_with(b"bplist00"));
        let back = decode(&bytes).unwrap();
        let bs = back.dict_get("bufferStream").unwrap();
        match bs {
            Value::Int(i) => {
                assert_eq!(i.width, 8);
                assert_eq!(i.bits as u64, bits);
                let s = i.display_probe();
                assert!(s.contains("0xf7fe018e00e80000"), "{s}");
                assert!(!s.contains("0xfffffffffffffffff7fe"), "{s}");
            }
            _ => panic!("expected int"),
        }
        assert_eq!(back.dict_get("name").unwrap().as_str(), Some("HomePod"));
    }

    #[test]
    fn plistlib_u64_high_bit_encodes_16_bytes() {
        // 2^63 .. 2^64-1 → marker 0x14
        let v = Value::Int(PlistInt {
            width: 16,
            bits: (1u128 << 63) + 1,
        });
        let bytes = encode(&v).unwrap();
        let back = decode(&bytes).unwrap();
        match back {
            Value::Int(i) => {
                assert_eq!(i.width, 16);
                assert_eq!(i.bits, (1u128 << 63) + 1);
            }
            _ => panic!("expected int"),
        }
    }

    #[test]
    fn lossy_utf8_ascii_path() {
        let v = Value::String("ok".into());
        let bytes = encode(&v).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back.as_str(), Some("ok"));
    }
}
