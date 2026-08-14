//! Optional TOML next to the exe. Keys match `docs/架构设计.md` §9.

#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub device_ip: String,
    pub device_name: String,
    pub capture_device: String,
    pub volume: f64,
    pub play: bool,
    pub sunshine_aware: bool,
    pub autostart: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(default)]
    ip: String,
    #[serde(default)]
    name: String,
}

#[derive(Default, Serialize, Deserialize)]
struct CaptureFile {
    #[serde(default)]
    device_name: String,
}

#[derive(Serialize, Deserialize)]
struct FileCfg {
    #[serde(default)]
    device: DeviceFile,
    #[serde(default)]
    capture: CaptureFile,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    play: bool,
    #[serde(default = "default_sunshine_aware")]
    sunshine_aware: bool,
    #[serde(default)]
    autostart: bool,
}

fn default_volume() -> f64 {
    0.5
}

fn default_sunshine_aware() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_ip: String::new(),
            device_name: String::new(),
            capture_device: String::new(),
            volume: 0.5,
            play: false,
            sunshine_aware: true,
            autostart: false,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("airplay.toml")))
            .unwrap_or_else(|| PathBuf::from("airplay.toml"))
    }

    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str::<FileCfg>(&s) {
                Ok(f) => Self {
                    device_ip: f.device.ip,
                    device_name: f.device.name,
                    capture_device: f.capture.device_name,
                    volume: f.volume.clamp(0.0, 1.0),
                    play: f.play,
                    sunshine_aware: f.sunshine_aware,
                    autostart: f.autostart,
                },
                Err(_) => Self::default(),
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let file = FileCfg {
            device: DeviceFile {
                ip: self.device_ip.clone(),
                name: self.device_name.clone(),
            },
            capture: CaptureFile {
                device_name: self.capture_device.clone(),
            },
            volume: self.volume.clamp(0.0, 1.0),
            play: self.play,
            sunshine_aware: self.sunshine_aware,
            autostart: self.autostart,
        };
        std::fs::write(path, toml::to_string_pretty(&file)?)?;
        Ok(())
    }
}
