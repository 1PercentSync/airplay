//! WASAPI loopback capture (Windows): shared-mode, event-driven, native mix
//! format (float32 stereo). Follows the Sunshine production chain.
//!
//! [evidence: research/04 §2-§3 (Sunshine audio.cpp:1014-1027 chain);
//!  windows-0.62.2 crate sources for exact signatures]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::PipeError;

/// Capture statistics (diagnostics contract).
#[derive(Default)]
pub struct CaptureStats {
    pub frames: AtomicU64,
    pub discontinuities: AtomicU64,
    pub empty_polls: AtomicU64,
}

/// Discovered endpoint: (device id string, friendly name).
pub struct Endpoint {
    pub id: String,
    pub name: String,
}

/// List active render endpoints (for selection UX and error messages).
pub fn list_endpoints() -> Result<Vec<Endpoint>, String> {
    use windows::Win32::Media::Audio::{eRender, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("CoCreateInstance: {e}"))?;
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| format!("EnumAudioEndpoints: {e}"))?;
        let count = collection.GetCount().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for i in 0..count {
            if let Ok(d) = collection.Item(i) {
                out.push(Endpoint {
                    id: device_id(&d),
                    name: friendly_name(&d),
                });
            }
        }
        Ok(out)
    }
}

/// Capture thread: loopback on the endpoint whose friendly name contains
/// `name_substring` (or the first candidate from the default list when
/// `None`). Emits f32 interleaved chunks of exactly `chunk_frames` frames
/// through `tx` until the channel closes.
pub fn capture_thread(
    name_substring: Option<String>,
    chunk_frames: usize,
    tx: std::sync::mpsc::Sender<Vec<f32>>,
    stats: Arc<CaptureStats>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), PipeError> {
    use windows::Win32::Media::Audio::{
        eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};

    const CANDIDATES: &[&str] = &["Steam Streaming Speakers", "VB-CABLE", "CABLE Input"];

    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| PipeError::Source(format!("CoInitializeEx: {e}")))?;

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| PipeError::Source(format!("CoCreateInstance: {e}")))?;
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| PipeError::Source(format!("EnumAudioEndpoints: {e}")))?;
        let count = collection.GetCount().map_err(|e| PipeError::Source(e.to_string()))?;

        // Select the endpoint.
        let mut chosen = None;
        let mut available = Vec::new();
        for i in 0..count {
            let Ok(d) = collection.Item(i) else { continue };
            let name = friendly_name(&d);
            available.push(name.clone());
            let matches = match &name_substring {
                Some(needle) => name.contains(needle.as_str()),
                None => CANDIDATES.iter().any(|c| name.contains(c)),
            };
            if matches && chosen.is_none() {
                chosen = Some(d);
            }
        }
        let device = chosen.ok_or_else(|| PipeError::NoDevice {
            candidates: name_substring
                .map(|s| vec![s])
                .unwrap_or_else(|| CANDIDATES.iter().map(|s| s.to_string()).collect()),
            available,
        })?;

        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| PipeError::Source(format!("Activate(IAudioClient): {e}")))?;
        let mix = client
            .GetMixFormat()
            .map_err(|e| PipeError::Source(format!("GetMixFormat: {e}")))?;
        let (rate, channels, float_ok) = parse_mix(mix);
        if !float_ok || channels != 2 {
            CoTaskMemFree(Some(mix as *const _));
            return Err(PipeError::UnsupportedFormat(format!(
                "{rate}Hz {channels}ch float={float_ok}"
            )));
        }

        // 30 ms shared buffer, event-driven.
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                300_000, // 30 ms in 100 ns units
                0,
                mix,
                None,
            )
            .map_err(|e| {
                CoTaskMemFree(Some(mix as *const _));
                PipeError::Source(format!("IAudioClient::Initialize(loopback): {e}"))
            })?;
        let event = CreateEventW(None, false, false, None)
            .map_err(|e| PipeError::Source(format!("CreateEventW: {e}")))?;
        client
            .SetEventHandle(event)
            .map_err(|e| PipeError::Source(format!("SetEventHandle: {e}")))?;
        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| PipeError::Source(format!("GetService(IAudioCaptureClient): {e}")))?;
        client
            .Start()
            .map_err(|e| PipeError::Source(format!("IAudioClient::Start: {e}")))?;

        let bytes_per_frame = 2 * 4usize; // f32 stereo
        let mut pending: Vec<f32> = Vec::with_capacity(chunk_frames * 4);
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            let w = WaitForSingleObject(event, 500);
            if w != WAIT_OBJECT_0 {
                stats.empty_polls.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            loop {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let hr = capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None);
                if hr.is_err() || frames == 0 {
                    let _ = capture.ReleaseBuffer(0);
                    break;
                }
                if flags & 0x1 != 0 {
                    // AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY
                    stats.discontinuities.fetch_add(1, Ordering::Relaxed);
                }
                let slice = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                pending.extend_from_slice(slice);
                stats.frames.fetch_add(frames as u64, Ordering::Relaxed);
                let _ = bytes_per_frame; // (documented layout: f32 × 2ch)
                if let Err(e) = capture.ReleaseBuffer(frames) {
                    let _ = e;
                }
            }
            while pending.len() >= chunk_frames * 2 {
                let chunk: Vec<f32> = pending.drain(..chunk_frames * 2).collect();
                if tx.send(chunk).is_err() {
                    client.Stop().ok();
                    CloseHandle(event).ok();
                    CoTaskMemFree(Some(mix as *const _));
                    return Ok(());
                }
            }
        }
        client.Stop().ok();
        CloseHandle(event).ok();
        CoTaskMemFree(Some(mix as *const _));
        Ok(())
    }
}

fn parse_mix(mix: *const windows::Win32::Media::Audio::WAVEFORMATEX) -> (u32, u16, bool) {
    use windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE;
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
    unsafe {
        let f = &*mix;
        let rate = f.nSamplesPerSec;
        let channels = f.nChannels;
        let tag = f.wFormatTag;
        let float_ok = if tag == WAVE_FORMAT_EXTENSIBLE as u16 {
            let ext = &*(mix as *const WAVEFORMATEXTENSIBLE);
            let sub = ext.SubFormat;
            sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            tag as u32 == WAVE_FORMAT_IEEE_FLOAT
        };
        (rate, channels, float_ok)
    }
}

fn friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> String {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .ok()
            .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName).ok())
            .and_then(|pv| PropVariantToStringAlloc(&pv).ok())
            .map(|pw| {
                let s = pw.to_string().unwrap_or_default();
                CoTaskMemFree(Some(pw.0 as *const _));
                s
            })
            .unwrap_or_else(|| "<unknown>".into())
    }
}

fn device_id(device: &windows::Win32::Media::Audio::IMMDevice) -> String {
    use windows::Win32::System::Com::CoTaskMemFree;
    unsafe {
        match device.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}
