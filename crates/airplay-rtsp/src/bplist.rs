//! Minimal binary plist (bplist00) codec.
//!
//! Decoder covers the full object type set; encoder covers the subset we
//! emit (dict/array/string/int/real/bool/data/date). Self-contained: our
//! SETUP bodies are produced by this encoder, `/info` replies are parsed
//! by this decoder.
//!
//! [evidence: format per Apple CFBinaryPList + airplay-cli/src/ap2_bplist.cpp]

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Real(f64),
    /// CFAbsoluteTime: seconds since 2001-01-01 00:00:00 UTC.
    Date(f64),
    Data(Vec<u8>),
    String(String),
    Uid(u64),
    Array(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

#[derive(Debug, thiserror::Error)]
pub enum BplistError {
    #[error("bad header")]
    BadHeader,
    #[error("truncated at offset {0}")]
    Truncated(usize),
    #[error("unsupported object marker 0x{0:02x} at offset {1}")]
    UnsupportedMarker(u8, usize),
    #[error("invalid reference {0} (objects: {1})")]
    BadRef(usize, usize),
    #[error("trailer/offset table malformed")]
    BadTrailer,
    #[error("utf8/utf16 decode error")]
    BadString,
}

const HEADER: &[u8; 8] = b"bplist00";

// ---------- Decoder ----------

pub fn decode(data: &[u8]) -> Result<Value, BplistError> {
    if data.len() < 8 + 32 || &data[..8] != HEADER {
        return Err(BplistError::BadHeader);
    }
    let t = data.len() - 32;
    let offset_int_size = data[t + 6] as usize;
    let object_ref_size = data[t + 7] as usize;
    let num_objects = be_u64(&data[t + 8..t + 16]) as usize;
    let top_object = be_u64(&data[t + 16..t + 24]) as usize;
    let offset_table_off = be_u64(&data[t + 24..t + 32]) as usize;
    if offset_int_size == 0 || offset_int_size > 8 || object_ref_size == 0 || object_ref_size > 8 {
        return Err(BplistError::BadTrailer);
    }
    if offset_table_off + num_objects * offset_int_size > t {
        return Err(BplistError::BadTrailer);
    }
    let mut offsets = Vec::with_capacity(num_objects);
    for i in 0..num_objects {
        let s = offset_table_off + i * offset_int_size;
        offsets.push(be_uint(&data[s..s + offset_int_size]) as usize);
    }
    let mut ctx = DecodeCtx {
        data,
        offsets: &offsets,
        object_ref_size,
    };
    if top_object >= num_objects {
        return Err(BplistError::BadRef(top_object, num_objects));
    }
    ctx.object(top_object)
}

struct DecodeCtx<'a> {
    data: &'a [u8],
    offsets: &'a [usize],
    object_ref_size: usize,
}

impl<'a> DecodeCtx<'a> {
    fn object(&mut self, idx: usize) -> Result<Value, BplistError> {
        if idx >= self.offsets.len() {
            return Err(BplistError::BadRef(idx, self.offsets.len()));
        }
        let start = self.offsets[idx];
        if start >= self.data.len() {
            return Err(BplistError::Truncated(start));
        }
        let marker = self.data[start];
        let kind = marker >> 4;
        let info = marker & 0x0F;
        let mut pos = start + 1;

        // Count helper: 0x0F means the next object is an int carrying the count.
        let mut count = |ctx: &mut Self, info: u8| -> Result<usize, BplistError> {
            if info != 0x0F {
                return Ok(info as usize);
            }
            if pos >= ctx.data.len() {
                return Err(BplistError::Truncated(pos));
            }
            let im = ctx.data[pos];
            if im >> 4 != 0x1 {
                return Err(BplistError::UnsupportedMarker(im, pos));
            }
            let nbytes = 1usize << (im & 0x0F);
            pos += 1;
            if pos + nbytes > ctx.data.len() {
                return Err(BplistError::Truncated(pos));
            }
            let v = be_uint(&ctx.data[pos..pos + nbytes]) as usize;
            pos += nbytes;
            Ok(v)
        };

        match (kind, info) {
            (0x0, 0x0) => Ok(Value::Null),
            (0x0, 0x8) => Ok(Value::Bool(false)),
            (0x0, 0x9) => Ok(Value::Bool(true)),
            (0x0, 0xF) => Ok(Value::Null), // fill
            (0x1, _) => {
                let nbytes = 1usize << info;
                if pos + nbytes > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Int(be_i128(&self.data[pos..pos + nbytes])))
            }
            (0x2, 2) => {
                if pos + 4 > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Real(f32::from_be_bytes(
                    self.data[pos..pos + 4].try_into().unwrap(),
                ) as f64))
            }
            (0x2, 3) => {
                if pos + 8 > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Real(f64::from_be_bytes(
                    self.data[pos..pos + 8].try_into().unwrap(),
                )))
            }
            (0x3, 3) => {
                if pos + 8 > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Date(f64::from_be_bytes(
                    self.data[pos..pos + 8].try_into().unwrap(),
                )))
            }
            (0x4, _) => {
                let n = count(self, info)?;
                if pos + n > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Data(self.data[pos..pos + n].to_vec()))
            }
            (0x5, _) => {
                let n = count(self, info)?;
                if pos + n > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                String::from_utf8(self.data[pos..pos + n].to_vec())
                    .map(Value::String)
                    .map_err(|_| BplistError::BadString)
            }
            (0x6, _) => {
                let n = count(self, info)?;
                if pos + n * 2 > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                let units: Vec<u16> = (0..n)
                    .map(|i| u16::from_be_bytes([self.data[pos + 2 * i], self.data[pos + 2 * i + 1]]))
                    .collect();
                String::from_utf16(&units)
                    .map(Value::String)
                    .map_err(|_| BplistError::BadString)
            }
            (0x8, _) => {
                let nbytes = info as usize + 1;
                if pos + nbytes > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                Ok(Value::Uid(be_uint(&self.data[pos..pos + nbytes])))
            }
            (0xA, _) => {
                let n = count(self, info)?;
                if pos + n * self.object_ref_size > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                let mut items = Vec::with_capacity(n);
                for i in 0..n {
                    let s = pos + i * self.object_ref_size;
                    let r = be_uint(&self.data[s..s + self.object_ref_size]) as usize;
                    items.push(self.object(r)?);
                }
                Ok(Value::Array(items))
            }
            (0xD, _) => {
                let n = count(self, info)?;
                if pos + 2 * n * self.object_ref_size > self.data.len() {
                    return Err(BplistError::Truncated(pos));
                }
                let mut map = BTreeMap::new();
                let keys_pos = pos;
                let vals_pos = pos + n * self.object_ref_size;
                for i in 0..n {
                    let ks = keys_pos + i * self.object_ref_size;
                    let vs = vals_pos + i * self.object_ref_size;
                    let kr = be_uint(&self.data[ks..ks + self.object_ref_size]) as usize;
                    let vr = be_uint(&self.data[vs..vs + self.object_ref_size]) as usize;
                    let key = match self.object(kr)? {
                        Value::String(s) => s,
                        _ => return Err(BplistError::UnsupportedMarker(0xD0, ks)),
                    };
                    let val = self.object(vr)?;
                    map.insert(key, val);
                }
                Ok(Value::Dict(map))
            }
            _ => Err(BplistError::UnsupportedMarker(marker, start)),
        }
    }
}

fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b.try_into().unwrap())
}

fn be_uint(b: &[u8]) -> u64 {
    let mut v = 0u64;
    for &x in b {
        v = (v << 8) | x as u64;
    }
    v
}

fn be_i128(b: &[u8]) -> i128 {
    let mut v: i128 = 0;
    for &x in b {
        v = (v << 8) | x as i128;
    }
    v
}

// ---------- Encoder (subset) ----------

/// Flattened object graph: children are referenced by global index so the
/// reference size can be fixed after the full count is known.
enum ObjDesc {
    Leaf(Vec<u8>),
    Array(Vec<usize>),
    Dict(Vec<usize>, Vec<usize>),
}

pub fn encode(value: &Value) -> Vec<u8> {
    let mut descs: Vec<ObjDesc> = Vec::new();
    let top = flatten(value, &mut descs);

    let ref_size = ref_size_for(descs.len());
    let objects: Vec<Vec<u8>> = descs.iter().map(|d| serialize_desc(d, ref_size)).collect();

    let mut body = Vec::new();
    let mut offsets: Vec<u64> = Vec::with_capacity(objects.len());
    for o in &objects {
        offsets.push((8 + body.len()) as u64);
        body.extend_from_slice(o);
    }
    let max_off = offsets.iter().max().copied().unwrap_or(0);
    let offset_size = ref_size_for(max_off as usize + 1);
    let offset_table_off = 8 + body.len();

    let mut out = Vec::with_capacity(offset_table_off + objects.len() * offset_size + 32);
    out.extend_from_slice(HEADER);
    out.extend_from_slice(&body);
    for off in &offsets {
        push_be_uint(&mut out, *off, offset_size);
    }
    // Trailer
    out.extend_from_slice(&[0u8; 6]);
    out.push(offset_size as u8);
    out.push(ref_size as u8);
    out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
    out.extend_from_slice(&(top as u64).to_be_bytes());
    out.extend_from_slice(&(offset_table_off as u64).to_be_bytes());
    out
}

fn serialize_desc(desc: &ObjDesc, ref_size: usize) -> Vec<u8> {
    match desc {
        ObjDesc::Leaf(b) => b.clone(),
        ObjDesc::Array(refs) => {
            let mut out = Vec::new();
            push_count(&mut out, 0xA, refs.len());
            for r in refs {
                push_be_uint(&mut out, *r as u64, ref_size);
            }
            out
        }
        ObjDesc::Dict(keys, vals) => {
            let mut out = Vec::new();
            push_count(&mut out, 0xD, keys.len());
            for r in keys.iter().chain(vals.iter()) {
                push_be_uint(&mut out, *r as u64, ref_size);
            }
            out
        }
    }
}

/// Append value (children first) into `descs`, returning this object's index.
fn flatten(value: &Value, descs: &mut Vec<ObjDesc>) -> usize {
    let desc = match value {
        Value::Array(items) => {
            let refs: Vec<usize> = items.iter().map(|v| flatten(v, descs)).collect();
            ObjDesc::Array(refs)
        }
        Value::Dict(map) => {
            let mut keys = Vec::with_capacity(map.len());
            let mut vals = Vec::with_capacity(map.len());
            for (k, v) in map {
                keys.push(flatten(&Value::String(k.clone()), descs));
                vals.push(flatten(v, descs));
            }
            ObjDesc::Dict(keys, vals)
        }
        _ => ObjDesc::Leaf(leaf_bytes(value)),
    };
    descs.push(desc);
    descs.len() - 1
}

fn leaf_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    match value {
        Value::Null => out.push(0x00),
        Value::Bool(false) => out.push(0x08),
        Value::Bool(true) => out.push(0x09),
        Value::Int(v) => {
            let (marker, bytes) = int_bytes(*v);
            out.push(0x10 | marker);
            out.extend_from_slice(&bytes);
        }
        Value::Real(f) => {
            out.push(0x23);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::Date(f) => {
            out.push(0x33);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::Data(d) => {
            push_count(&mut out, 0x4, d.len());
            out.extend_from_slice(d);
        }
        Value::String(s) => {
            if s.is_ascii() {
                push_count(&mut out, 0x5, s.len());
                out.extend_from_slice(s.as_bytes());
            } else {
                let units: Vec<u16> = s.encode_utf16().collect();
                push_count(&mut out, 0x6, units.len());
                for u in units {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
        }
        Value::Uid(u) => {
            let bytes = u.to_be_bytes();
            let first = bytes.iter().position(|&b| b != 0).unwrap_or(7);
            let n = (8 - first).max(1);
            out.push(0x80 | (n as u8 - 1));
            out.extend_from_slice(&bytes[8 - n..]);
        }
        Value::Array(_) | Value::Dict(_) => unreachable!("handled by flatten"),
    }
    out
}

/// Bytes needed to reference indices `0..max_index_plus_one-1`.
fn ref_size_for(max_index_plus_one: usize) -> usize {
    let mut n = 1;
    let mut cap = 256usize;
    while cap < max_index_plus_one {
        n += 1;
        cap = cap.saturating_mul(256);
    }
    n
}

fn push_be_uint(out: &mut Vec<u8>, v: u64, size: usize) {
    let b = v.to_be_bytes();
    out.extend_from_slice(&b[8 - size..]);
}

fn push_count(out: &mut Vec<u8>, kind: u8, n: usize) {
    if n < 15 {
        out.push((kind << 4) | n as u8);
    } else {
        out.push((kind << 4) | 0x0F);
        // The count int is written inline (not as a referenced object).
        let (marker, bytes) = int_bytes(n as i128);
        out.push(0x10 | marker);
        out.extend_from_slice(&bytes);
    }
}

fn int_bytes(v: i128) -> (u8, Vec<u8>) {
    if (0..=0xFF).contains(&v) {
        (0, vec![v as u8])
    } else if (0..=0xFFFF).contains(&v) {
        (1, (v as u16).to_be_bytes().to_vec())
    } else if (0..=0xFFFF_FFFF).contains(&v) {
        (2, (v as u32).to_be_bytes().to_vec())
    } else if v >= 0 && v <= u64::MAX as i128 {
        (3, (v as u64).to_be_bytes().to_vec())
    } else {
        (4, v.to_be_bytes().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        let mut d = BTreeMap::new();
        d.insert("name".into(), Value::String("Living Room".into()));
        d.insert("features".into(), Value::Int(0x4A7FCA00));
        d.insert("big".into(), Value::Int(0x1_0000_0000));
        d.insert("huge".into(), Value::Int(0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10));
        d.insert("neg".into(), Value::Int(-144));
        d.insert("real".into(), Value::Real(-30.0));
        d.insert("flag".into(), Value::Bool(true));
        d.insert("off".into(), Value::Bool(false));
        d.insert("blob".into(), Value::Data(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        d.insert(
            "list".into(),
            Value::Array(vec![Value::Int(1), Value::String("two".into())]),
        );
        let mut sub = BTreeMap::new();
        sub.insert(
            "audioStream".into(),
            Value::Array(vec![Value::Int(0x40000), Value::Int(0x100000)]),
        );
        d.insert("supportedAudioFormatsExtended".into(), Value::Dict(sub));
        d.insert("uni".into(), Value::String("客厅".into()));
        Value::Dict(d)
    }

    #[test]
    fn roundtrip() {
        let v = sample();
        let bytes = encode(&v);
        assert_eq!(&bytes[..8], b"bplist00");
        let back = decode(&bytes).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn large_data_roundtrip() {
        let v = Value::Data((0..600u32).map(|i| (i % 251) as u8).collect());
        let bytes = encode(&v);
        assert_eq!(decode(&bytes).unwrap(), v);
    }

    #[test]
    fn many_objects_roundtrip() {
        // >255 objects forces 2-byte references.
        let v = Value::Array((0..300i128).map(Value::Int).collect());
        let bytes = encode(&v);
        assert_eq!(decode(&bytes).unwrap(), v);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"not a plist at all........................").is_err());
    }
}
