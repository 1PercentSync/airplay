use airplay_core::Result;
#[cfg(not(windows))]
use airplay_core::Error;

#[derive(Clone, Debug)]
pub struct RenderDevice {
    pub id: String,
    pub friendly_name: String,
    pub mix_rate: u32,
    pub mix_channels: u16,
    pub mix_bits: u16,
    pub mix_valid_bits: u16,
    pub subtype: String,
}

#[cfg(not(windows))]
pub fn list_render_devices() -> Result<Vec<RenderDevice>> {
    Err(Error::Audio(
        "WASAPI enumeration is Windows-only (build and run on Windows)".into(),
    ))
}

#[cfg(windows)]
pub fn list_render_devices() -> Result<Vec<RenderDevice>> {
    windows_enum::list_render_devices()
}

/// [evidence: Sunshine audio.cpp:1119-1146 GetId/FriendlyName;
/// Sunshine audio.cpp:373 GetMixFormat; tools/audio.cpp:163-227, 276-307]
#[cfg(windows)]
mod windows_enum {
    use super::RenderDevice;
    use airplay_core::{Error, Result};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::{
        eRender, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };

    pub fn list_render_devices() -> Result<Vec<RenderDevice>> {
        unsafe { list_render_devices_inner() }
    }

    unsafe fn list_render_devices_inner() -> Result<Vec<RenderDevice>> {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| Error::Audio(format!("CoInitializeEx: {e}")))?;
        let result = list_after_com();
        CoUninitialize();
        result
    }

    unsafe fn list_after_com() -> Result<Vec<RenderDevice>> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| Error::Audio(format!("MMDeviceEnumerator: {e}")))?;
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::Audio(format!("EnumAudioEndpoints: {e}")))?;
        let count = collection
            .GetCount()
            .map_err(|e| Error::Audio(format!("GetCount: {e}")))?;
        let mut out = Vec::new();
        for i in 0..count {
            let device = collection
                .Item(i)
                .map_err(|e| Error::Audio(format!("Item {i}: {e}")))?;
            let id_pwstr = device
                .GetId()
                .map_err(|e| Error::Audio(format!("GetId: {e}")))?;
            let id = id_pwstr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(id_pwstr.0 as *const core::ffi::c_void as *mut _));

            let mut friendly = String::from("(unnamed)");
            if let Ok(store) = device.OpenPropertyStore(STGM_READ) {
                if let Ok(pv) = store.GetValue(&PKEY_Device_FriendlyName) {
                    let s = pv.to_string();
                    if !s.is_empty() {
                        friendly = s;
                    }
                }
            }

            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| Error::Audio(format!("Activate IAudioClient: {e}")))?;
            let fmt_ptr = client
                .GetMixFormat()
                .map_err(|e| Error::Audio(format!("GetMixFormat: {e}")))?;
            if fmt_ptr.is_null() {
                return Err(Error::Audio("GetMixFormat returned null".into()));
            }
            let fmt: &WAVEFORMATEX = &*fmt_ptr;
            let mix_rate = fmt.nSamplesPerSec;
            let mix_channels = fmt.nChannels;
            let mix_bits = fmt.wBitsPerSample;
            let tag = fmt.wFormatTag;
            let mut mix_valid_bits = mix_bits;
            let mut subtype = match tag {
                0x0001 => "pcm".to_string(),
                0x0003 => "float".to_string(),
                0xFFFE => "extensible".to_string(),
                other => format!("tag=0x{other:04x}"),
            };
            if tag == 0xFFFE {
                let ext = &*(fmt_ptr as *const WAVEFORMATEXTENSIBLE);
                let valid = ext.Samples.wValidBitsPerSample;
                mix_valid_bits = valid;
                let g = ext.SubFormat;
                const FLOAT: u128 = 0x0000_0003_0000_0010_8000_00aa_0038_9b71;
                const PCM: u128 = 0x0000_0001_0000_0010_8000_00aa_0038_9b71;
                subtype = if g == windows::core::GUID::from_u128(FLOAT) {
                    "float".into()
                } else if g == windows::core::GUID::from_u128(PCM) {
                    "pcm".into()
                } else {
                    format!("{g:?}")
                };
            }
            CoTaskMemFree(Some(fmt_ptr as *const core::ffi::c_void as *mut _));
            out.push(RenderDevice {
                id,
                friendly_name: friendly,
                mix_rate,
                mix_channels,
                mix_bits,
                mix_valid_bits,
                subtype,
            });
        }
        Ok(out)
    }
}
