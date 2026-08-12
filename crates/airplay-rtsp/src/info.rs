//! `/info` bplist parsing and capability summary.
//!
//! Capability interpretation (features/statusFlags bit semantics):
//! [evidence: research/02-协议调研.md §1.1 (pyatv FtFlag mapping);
//!  airplay-cli/src/ap2_client.c:1339-1357 (format tables under
//!  supportedAudioFormatsExtended.{audioStream,bufferStream} / supportedFormats)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::bplist::{self, Value};

pub struct Info {
    pub root: Value,
}

impl Info {
    pub fn parse(body: &[u8]) -> Result<Self, bplist::BplistError> {
        Ok(Self {
            root: bplist::decode(body)?,
        })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match &self.root {
            Value::Dict(m) => m.get(key),
            _ => None,
        }
    }

    /// Interesting scalar capabilities with one-line interpretation.
    pub fn capability_summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        for key in [
            "deviceID",
            "model",
            "name",
            "sourceVersion",
            "protocolVersion",
            "manufacturer",
        ] {
            if let Some(v) = self.get(key) {
                out.push(format!("{key} = {}", brief(v)));
            }
        }
        if let Some(Value::Int(f)) = self.get("features") {
            out.push(format!("features = 0x{f:X}"));
            out.push(format!("  {}", decode_features(*f as u64)));
        }
        if let Some(Value::Int(f)) = self.get("statusFlags") {
            out.push(format!("statusFlags = 0x{f:X}"));
            out.push(format!("  {}", decode_status_flags(*f as u64)));
        }
        if let Some(v) = self.get("pw") {
            out.push(format!("pw (password required) = {}", brief(v)));
        }
        if let Some(v) = self.get("pk") {
            out.push(format!("pk (HAP pubkey present) = {}", brief(v)));
        }
        // Format tables (evidence: ap2_parse_format_capability).
        for key in ["supportedAudioFormatsExtended", "supportedFormats"] {
            if let Some(Value::Dict(m)) = self.get(key) {
                for (stream, v) in m {
                    out.push(format!("{key}.{stream} = {}", format_mask(v)));
                }
            }
        }
        out
    }

    /// Full key dump (truncated values) for logs.
    pub fn dump(&self) -> String {
        let mut s = String::new();
        if let Value::Dict(m) = &self.root {
            dump_dict(&mut s, m, 0);
        }
        s
    }
}

fn dump_dict(s: &mut String, m: &BTreeMap<String, Value>, indent: usize) {
    for (k, v) in m {
        let pad = "  ".repeat(indent);
        match v {
            Value::Dict(sub) => {
                let _ = writeln!(s, "{pad}{k}:");
                dump_dict(s, sub, indent + 1);
            }
            Value::Array(items) => {
                let _ = writeln!(s, "{pad}{k}: array[{}]", items.len());
                for (i, it) in items.iter().enumerate() {
                    if let Value::Dict(sub) = it {
                        let _ = writeln!(s, "{pad}  [{i}]:");
                        dump_dict(s, sub, indent + 2);
                    } else {
                        let _ = writeln!(s, "{pad}  [{i}] = {}", brief(it));
                    }
                }
            }
            _ => {
                let _ = writeln!(s, "{pad}{k} = {}", brief(v));
            }
        }
    }
}

/// Short human-readable rendering of a value (truncates long payloads).
pub fn brief(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => format!("{b}"),
        Value::Int(i) => format!("{i}"),
        Value::Real(f) => format!("{f}"),
        Value::Date(d) => format!("date({d})"),
        Value::Data(d) => {
            if d.len() <= 24 {
                format!("data[{}] {}", d.len(), hex(d))
            } else {
                format!("data[{}] {}…", d.len(), hex(&d[..24]))
            }
        }
        Value::String(s) => format!("\"{s}\""),
        Value::Uid(u) => format!("uid({u})"),
        Value::Array(a) => format!("array[{}]", a.len()),
        Value::Dict(_) => "dict".into(),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Render a format mask (Int or array of Ints) as a bitmask + decoded codes.
fn format_mask(v: &Value) -> String {
    let ints: Vec<i128> = match v {
        Value::Int(i) => vec![*i],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| match x {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .collect(),
        _ => return brief(v),
    };
    let mut mask: u128 = 0;
    for i in &ints {
        if *i >= 0 && *i < 128 {
            mask |= 1u128 << *i;
        } else {
            // Might itself be a packed mask.
            mask |= *i as u128;
        }
    }
    let known: Vec<String> = [0x40000u64, 0x80000, 0x100000, 0x200000]
        .iter()
        .filter(|c| mask as u64 & **c != 0)
        .map(|c| match airplay_core::AlacFormat::from_code(*c) {
            Some(f) => format!(
                "{}kHz/{}bit (0x{c:X})",
                f.sample_rate() as f32 / 1000.0,
                f.bit_depth()
            ),
            None => format!("0x{c:X}"),
        })
        .collect();
    format!(
        "raw={ints:?} → mask=0x{mask:X}{}",
        if known.is_empty() {
            String::new()
        } else {
            format!(" [{}]", known.join(", "))
        }
    )
}

/// [evidence: research/02 §1.1 FtFlag bit positions]
fn decode_features(f: u64) -> String {
    let mut bits = Vec::new();
    let named: &[(u8, &str)] = &[
        (0, "Video"),
        (1, "Photo"),
        (7, "AirPlay"),
        (9, "Audio"),
        (18, "AudioFormats_0"),
        (19, "AudioFormats_1"),
        (20, "AudioFormats_2"),
        (21, "AudioFormats_3"),
        (38, "AirPlay2"),
        (40, "BufferedAudio"),
        (41, "PTP"),
        (46, "HKPairing"),
        (48, "UnifiedScreen"),
    ];
    for (b, name) in named {
        if f >> b & 1 == 1 {
            bits.push(format!("bit{b}={name}"));
        }
    }
    format!("set bits: {}", if bits.is_empty() { "none".into() } else { bits.join(", ") })
}

fn decode_status_flags(f: u64) -> String {
    let mut bits = Vec::new();
    let named: &[(u8, &str)] = &[
        (0, "Unknown0"),
        (1, "DeviceNotConfigured?"),
        (2, "PinRequired"),
        (3, "Unknown3"),
        (4, "PasswordRequired"),
    ];
    for (b, name) in named {
        if f >> b & 1 == 1 {
            bits.push(format!("bit{b}={name}"));
        }
    }
    format!("set bits: {}", if bits.is_empty() { "none".into() } else { bits.join(", ") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bplist;

    #[test]
    fn parses_and_summarizes() {
        let mut m = BTreeMap::new();
        m.insert("deviceID".into(), Value::String("AA:BB:CC:DD:EE:FF".into()));
        m.insert("model".into(), Value::String("AudioAccessory5,1".into()));
        m.insert("features".into(), Value::Int(0x4A7FCA00));
        m.insert("statusFlags".into(), Value::Int(0x204));
        let mut fmt = BTreeMap::new();
        fmt.insert(
            "audioStream".into(),
            Value::Array(vec![Value::Int(0x40000), Value::Int(0x100000)]),
        );
        m.insert("supportedAudioFormatsExtended".into(), Value::Dict(fmt));
        let body = bplist::encode(&Value::Dict(m));

        let info = Info::parse(&body).unwrap();
        let s = info.capability_summary().join("\n");
        assert!(s.contains("AudioAccessory5,1"));
        assert!(s.contains("features = 0x4A7FCA00"));
        assert!(s.contains("44.1kHz/16bit"));
        assert!(s.contains("48kHz/16bit"));
    }
}
