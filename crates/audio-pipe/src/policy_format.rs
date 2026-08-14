//! Shared-mode mix format: set 44.1 kHz on start, restore on drop.
//!
//! [evidence: Sunshine audio.cpp:996-1020 SetDeviceFormat;
//! Sunshine PolicyConfig.h IPolicyConfig / CPolicyConfigClient]

#![cfg(windows)]

use std::ffi::c_void;
use tracing::{info, warn};
use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::HSTRING;

const CLSID_CPOLICY_CONFIG_CLIENT: GUID =
    GUID::from_u128(0x870a_f99c_171d_4f9e_af0d_e63d_f40c_2bc9);
const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xf867_9f50_850a_41cf_9c72_430f_2902_90c8);
const CLSCTX_ALL: u32 = 0x17;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SPEAKER_STEREO: u32 = 0x3;
const FLOAT_GUID: GUID = GUID::from_u128(0x0000_0003_0000_0010_8000_00aa_0038_9b71);
const PCM_GUID: GUID = GUID::from_u128(0x0000_0001_0000_0010_8000_00aa_0038_9b71);

#[link(name = "ole32")]
extern "system" {
    fn CoCreateInstance(
        rclsid: *const GUID,
        punkouter: *mut c_void,
        dwclscontext: u32,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> i32;
}

#[repr(C)]
struct IPolicyConfig {
    vtbl: *const IPolicyConfigVtbl,
}

#[repr(C)]
struct IPolicyConfigVtbl {
    query_interface: unsafe extern "system" fn(*mut IPolicyConfig, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut IPolicyConfig) -> u32,
    release: unsafe extern "system" fn(*mut IPolicyConfig) -> u32,
    get_mix_format: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *mut *mut WAVEFORMATEX) -> i32,
    get_device_format:
        unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, i32, *mut *mut WAVEFORMATEX) -> i32,
    reset_device_format: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR) -> i32,
    set_device_format: unsafe extern "system" fn(
        *mut IPolicyConfig,
        PCWSTR,
        *mut WAVEFORMATEX,
        *mut WAVEFORMATEX,
    ) -> i32,
    get_processing_period:
        unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, i32, *mut i64, *mut i64) -> i32,
    set_processing_period: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *mut i64) -> i32,
    get_share_mode: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *mut c_void) -> i32,
    set_share_mode: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *mut c_void) -> i32,
    get_property_value: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *const c_void, *mut c_void) -> i32,
    set_property_value: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, *const c_void, *mut c_void) -> i32,
    set_default_endpoint: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, i32) -> i32,
    set_endpoint_visibility: unsafe extern "system" fn(*mut IPolicyConfig, PCWSTR, i32) -> i32,
}

pub struct FormatGuard {
    id: String,
    saved: Option<Vec<u8>>,
}

impl FormatGuard {
    /// Set shared mix to 44.1 kHz stereo. Restore previous device format on drop.
    pub fn apply(device_id: &str) -> Self {
        ensure_com();
        let Some(policy) = Policy::new() else {
            warn!("IPolicyConfig unavailable, mix format unchanged");
            return Self {
                id: device_id.into(),
                saved: None,
            };
        };
        let id = HSTRING::from(device_id);
        let saved = policy.get_device_format(&id);
        let current_rate = saved
            .as_ref()
            .and_then(|b| wfex_rate(b))
            .unwrap_or(0);
        if current_rate == 44100 {
            info!(rate = current_rate, "mix already 44100, not changing");
            return Self {
                id: device_id.into(),
                saved: None,
            };
        }
        if policy.set_44100(&id, saved.as_deref()) {
            info!(from = current_rate, "set mix format 44100; will restore on stop");
            Self {
                id: device_id.into(),
                saved,
            }
        } else {
            warn!(from = current_rate, "SetDeviceFormat 44100 failed, leaving mix as-is");
            Self {
                id: device_id.into(),
                saved: None,
            }
        }
    }
}

impl Drop for FormatGuard {
    fn drop(&mut self) {
        let Some(bytes) = self.saved.take() else {
            return;
        };
        ensure_com();
        let Some(policy) = Policy::new() else {
            warn!("IPolicyConfig gone, cannot restore mix format");
            return;
        };
        let id = HSTRING::from(self.id.as_str());
        let current_rate = policy
            .get_device_format(&id)
            .as_ref()
            .and_then(|b| wfex_rate(b));
        if current_rate != Some(44100) {
            info!(
                ?current_rate,
                "mix format no longer 44100, not restoring"
            );
            return;
        }
        if policy.set_raw(&id, &bytes) {
            info!("restored previous mix format");
        } else {
            warn!("failed to restore previous mix format");
        }
    }
}

fn ensure_com() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
}

struct Policy {
    ptr: *mut IPolicyConfig,
}

impl Policy {
    fn new() -> Option<Self> {
        unsafe {
            let mut pv: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_CPOLICY_CONFIG_CLIENT,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IID_IPOLICY_CONFIG,
                &mut pv,
            );
            if hr < 0 || pv.is_null() {
                warn!(hr, "CoCreateInstance CPolicyConfigClient failed");
                return None;
            }
            Some(Self {
                ptr: pv as *mut IPolicyConfig,
            })
        }
    }

    fn get_device_format(&self, id: &HSTRING) -> Option<Vec<u8>> {
        unsafe {
            let vtbl = (*self.ptr).vtbl;
            let mut p: *mut WAVEFORMATEX = std::ptr::null_mut();
            let hr = ((*vtbl).get_device_format)(self.ptr, PCWSTR(id.as_ptr()), 0, &mut p);
            if hr < 0 || p.is_null() {
                let hr2 = ((*vtbl).get_mix_format)(self.ptr, PCWSTR(id.as_ptr()), &mut p);
                if hr2 < 0 || p.is_null() {
                    return None;
                }
            }
            let n = std::mem::size_of::<WAVEFORMATEX>() + (*p).cbSize as usize;
            let bytes = std::slice::from_raw_parts(p as *const u8, n).to_vec();
            CoTaskMemFree(Some(p as *const c_void as *mut _));
            Some(bytes)
        }
    }

    fn set_44100(&self, id: &HSTRING, previous: Option<&[u8]>) -> bool {
        let bits = previous.and_then(wfex_bits).unwrap_or(32);
        let valid = previous.and_then(wfex_valid_bits).unwrap_or(bits);
        let float = previous.map(wfex_is_float).unwrap_or(true);
        let mut candidates = Vec::new();
        candidates.push(extensible(44100, 2, bits, valid, float));
        candidates.push(extensible(44100, 2, 32, 32, true));
        candidates.push(extensible(44100, 2, 16, 16, false));
        candidates.push(extensible(44100, 2, 32, 24, false));
        candidates.push(extensible(44100, 2, 24, 24, false));
        candidates.push(extensible(44100, 2, 32, 32, false));
        for mut fmt in candidates {
            if self.set_wfex(id, &mut fmt) {
                return true;
            }
        }
        false
    }

    fn set_raw(&self, id: &HSTRING, bytes: &[u8]) -> bool {
        if bytes.len() < std::mem::size_of::<WAVEFORMATEX>() {
            return false;
        }
        let mut owned = bytes.to_vec();
        let p = owned.as_mut_ptr() as *mut WAVEFORMATEX;
        self.set_ptr(id, p)
    }

    fn set_wfex(&self, id: &HSTRING, fmt: &mut WAVEFORMATEXTENSIBLE) -> bool {
        self.set_ptr(id, &mut fmt.Format as *mut WAVEFORMATEX)
    }

    fn set_ptr(&self, id: &HSTRING, fmt: *mut WAVEFORMATEX) -> bool {
        unsafe {
            let vtbl = (*self.ptr).vtbl;
            let mut dummy: WAVEFORMATEXTENSIBLE = std::mem::zeroed();
            let hr = ((*vtbl).set_device_format)(
                self.ptr,
                PCWSTR(id.as_ptr()),
                fmt,
                &mut dummy.Format,
            );
            HRESULT(hr).is_ok()
        }
    }
}

impl Drop for Policy {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                let vtbl = (*self.ptr).vtbl;
                let _ = ((*vtbl).release)(self.ptr);
            }
        }
    }
}

fn extensible(rate: u32, ch: u16, bits: u16, valid: u16, float: bool) -> WAVEFORMATEXTENSIBLE {
    let block = ch * (bits / 8);
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE,
            nChannels: ch,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: {
            let mut s: windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE_0 =
                unsafe { std::mem::zeroed() };
            s.wValidBitsPerSample = valid;
            s
        },
        dwChannelMask: SPEAKER_STEREO,
        SubFormat: if float { FLOAT_GUID } else { PCM_GUID },
    }
}

fn wfex_rate(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 12 {
        return None;
    }
    Some(u32::from_le_bytes(bytes[4..8].try_into().ok()?))
}

fn wfex_bits(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 16 {
        return None;
    }
    Some(u16::from_le_bytes(bytes[14..16].try_into().ok()?))
}

fn wfex_valid_bits(bytes: &[u8]) -> Option<u16> {
    if bytes.len() >= 20 {
        Some(u16::from_le_bytes(bytes[18..20].try_into().ok()?))
    } else {
        wfex_bits(bytes)
    }
}

fn wfex_is_float(bytes: &[u8]) -> bool {
    if bytes.len() >= 40 {
        let g = &bytes[24..40];
        g[0] == 3 && g[1] == 0 && g[2] == 0 && g[3] == 0
    } else if bytes.len() >= 2 {
        u16::from_le_bytes([bytes[0], bytes[1]]) == 0x0003
    } else {
        true
    }
}
