//! `probe devices`: enumerate WASAPI render endpoints with their mix format.
//!
//! Real implementation is Windows-only; other platforms print a stub.
//!
//! Symbol paths verified against the windows-0.62.2 crate sources:
//! [evidence: .cargo-home/registry/src/*/windows-0.62.2/src/Windows/Win32/
//!  Media/Audio/mod.rs (IMMDevice::Activate/OpenPropertyStore/GetId,
//!  IAudioClient::GetMixFormat);
//!  System/Com/mod.rs (STGM_READ, CoTaskMemFree);
//!  System/Com/StructuredStorage/mod.rs:616 (PropVariantToStringAlloc);
//!  Media/KernelStreaming/mod.rs:8598 (WAVE_FORMAT_EXTENSIBLE);
//!  Media/Multimedia/mod.rs:3062 (KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
//!  Devices/FunctionDiscovery/mod.rs:2002 (PKEY_Device_FriendlyName)]

#[cfg(not(windows))]
pub fn run() -> i32 {
    println!("[STATUS] devices: only supported on Windows (build on the target machine)");
    1
}

#[cfg(windows)]
pub fn run() -> i32 {
    match enumerate() {
        Ok(devices) => {
            if devices.is_empty() {
                println!("[STATUS] devices: no active render endpoints found");
                return 1;
            }
            println!("[STATUS] devices: {} active render endpoint(s)", devices.len());
            for (i, d) in devices.iter().enumerate() {
                println!("--- [{i}] {} ---", d.name);
                println!("  id: {}", d.id);
                match &d.mix {
                    Some(m) => println!("  mix: {m}"),
                    None => println!("  mix: <unavailable>"),
                }
            }
            println!("[STATUS] devices_ok");
            0
        }
        Err(e) => {
            println!("[STATUS] devices_failed: {e}");
            1
        }
    }
}

#[cfg(windows)]
struct DeviceInfo {
    name: String,
    id: String,
    mix: Option<String>,
}

#[cfg(windows)]
fn enumerate() -> Result<Vec<DeviceInfo>, String> {
    use windows::Win32::Media::Audio::{
        eRender, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {e}"))?;

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| format!("CoCreateInstance(MMDeviceEnumerator): {e}"))?;

        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| format!("EnumAudioEndpoints: {e}"))?;
        let count = collection.GetCount().map_err(|e| format!("GetCount: {e}"))?;

        let mut out = Vec::new();
        for i in 0..count {
            match collection.Item(i) {
                Ok(device) => out.push(describe(&device)),
                Err(e) => {
                    out.push(DeviceInfo {
                        name: format!("<item {i} error: {e}>"),
                        id: String::new(),
                        mix: None,
                    });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(windows)]
fn describe(device: &windows::Win32::Media::Audio::IMMDevice) -> DeviceInfo {
    use windows::Win32::Media::Audio::IAudioClient;
    use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL};

    let name = device_friendly_name(device).unwrap_or_else(|| "<unknown>".into());
    let id = unsafe {
        match device.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_else(|_| "<bad utf16>".into());
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => "<no id>".into(),
        }
    };

    let mix = unsafe {
        device
            .Activate::<IAudioClient>(CLSCTX_ALL, None)
            .ok()
            .and_then(|client| client.GetMixFormat().ok())
            .map(|fmt| describe_wave_format(fmt))
    };

    DeviceInfo { name, id, mix }
}

#[cfg(windows)]
fn device_friendly_name(
    device: &windows::Win32::Media::Audio::IMMDevice,
) -> Option<String> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};

    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let pv = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let pw = PropVariantToStringAlloc(&pv).ok()?;
        let s = pw.to_string().ok()?;
        CoTaskMemFree(Some(pw.0 as *const _));
        Some(s)
    }
}

#[cfg(windows)]
unsafe fn describe_wave_format(
    fmt: *const windows::Win32::Media::Audio::WAVEFORMATEX,
) -> String {
    use windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE;
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;

    let f = &*fmt;
    // WAVEFORMATEX is #[repr(C, packed(1))] in windows 0.62 — copy fields to
    // locals before use (borrowing a packed field is E0793 / UB).
    let rate = f.nSamplesPerSec;
    let channels = f.nChannels;
    let bits = f.wBitsPerSample;
    let tag = f.wFormatTag;
    let base = format!("{rate}Hz {channels}ch {bits}bit tag=0x{tag:X}");
    let detail = if tag == WAVE_FORMAT_EXTENSIBLE as u16 {
        let ext = &*(fmt as *const WAVEFORMATEXTENSIBLE);
        let sub = ext.SubFormat;
        let kind = if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            "float"
        } else {
            "pcm/other"
        };
        let valid = ext.Samples.wValidBitsPerSample;
        format!("extensible {kind} valid_bits={valid}")
    } else {
        "pcm".to_string()
    };
    let s = format!("{base} ({detail})");
    windows::Win32::System::Com::CoTaskMemFree(Some(fmt as *const _));
    s
}
