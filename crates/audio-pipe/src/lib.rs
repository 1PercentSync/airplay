//! WASAPI render-endpoint enumeration (probe devices).
//!
//! `[evidence: Sunshine tools/audio.cpp:163-228 print_device; src/platform/windows/audio.cpp:353-388 GetMixFormat]`

use airplay_core::{Error, Result};

#[derive(Debug, Clone)]
pub struct RenderEndpoint {
    pub id: String,
    pub friendly_name: String,
    pub adapter_name: String,
    pub description: String,
    pub mix_format: String,
}

/// Enumerate active render endpoints and their mix formats.
pub fn enumerate_render_endpoints() -> Result<Vec<RenderEndpoint>> {
    #[cfg(windows)]
    {
        windows_impl::enumerate()
    }
    #[cfg(not(windows))]
    {
        Err(Error::Unsupported(
            "probe devices requires Windows (WASAPI)",
        ))
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use windows::core::GUID;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eRender, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
        STGM_READ,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

    // `[evidence: Sunshine tools/audio.cpp:18-20 DEFINE_PROPERTYKEY]`
    const PKEY_DEVICE_DEVICEDESC: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 2,
    };
    const PKEY_DEVICE_FRIENDLYNAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    const PKEY_DEVICEINTERFACE_FRIENDLYNAME: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x026e516e_b814_414b_83cd_856d6fef4822),
        pid: 2,
    };

    const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
        GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    pub fn enumerate() -> Result<Vec<RenderEndpoint>> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| Error::Audio(format!("MMDeviceEnumerator: {e}")))?;

            let collection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| Error::Audio(format!("EnumAudioEndpoints: {e}")))?;
            let count = collection
                .GetCount()
                .map_err(|e| Error::Audio(e.to_string()))?;

            let mut out = Vec::new();
            for i in 0..count {
                let device: IMMDevice = collection
                    .Item(i)
                    .map_err(|e| Error::Audio(e.to_string()))?;
                out.push(describe(&device)?);
            }
            Ok(out)
        }
    }

    unsafe fn describe(device: &IMMDevice) -> Result<RenderEndpoint> {
        let id = {
            let pwstr = device.GetId().map_err(|e| Error::Audio(e.to_string()))?;
            let s = if pwstr.is_null() {
                String::new()
            } else {
                pwstr.to_string().unwrap_or_default()
            };
            CoTaskMemFree(Some(pwstr.0 as *const _));
            s
        };

        let store: IPropertyStore = device
            .OpenPropertyStore(STGM_READ)
            .map_err(|e| Error::Audio(e.to_string()))?;
        let friendly_name = prop_string(&store, &PKEY_DEVICE_FRIENDLYNAME);
        let adapter_name = prop_string(&store, &PKEY_DEVICEINTERFACE_FRIENDLYNAME);
        let description = prop_string(&store, &PKEY_DEVICE_DEVICEDESC);
        let mix_format =
            mix_format_string(device).unwrap_or_else(|e| format!("(unavailable: {e})"));

        Ok(RenderEndpoint {
            id,
            friendly_name,
            adapter_name,
            description,
            mix_format,
        })
    }

    unsafe fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> String {
        let Ok(mut var) = store.GetValue(key) else {
            return String::new();
        };
        let s = {
            let inner = &*var.Anonymous.Anonymous;
            if inner.vt == VT_LPWSTR && !inner.Anonymous.pwszVal.is_null() {
                inner.Anonymous.pwszVal.to_string().unwrap_or_default()
            } else {
                String::new()
            }
        };
        let _ = PropVariantClear(&mut var);
        s
    }

    unsafe fn mix_format_string(device: &IMMDevice) -> Result<String> {
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| Error::Audio(format!("Activate IAudioClient: {e}")))?;
        let pwfx = client
            .GetMixFormat()
            .map_err(|e| Error::Audio(format!("GetMixFormat: {e}")))?;
        if pwfx.is_null() {
            return Err(Error::Audio("GetMixFormat returned null".into()));
        }
        let s = format_wave(pwfx);
        CoTaskMemFree(Some(pwfx as *const _));
        Ok(s)
    }

    unsafe fn format_wave(pwfx: *mut WAVEFORMATEX) -> String {
        let w = core::ptr::read_unaligned(pwfx);
        let format_tag = w.wFormatTag;
        let n_channels = w.nChannels;
        let n_samples_per_sec = w.nSamplesPerSec;
        let w_bits_per_sample = w.wBitsPerSample;
        let cb_size = w.cbSize;
        let mut tag = match format_tag {
            WAVE_FORMAT_PCM => "pcm".to_string(),
            WAVE_FORMAT_IEEE_FLOAT => "float".to_string(),
            WAVE_FORMAT_EXTENSIBLE => "extensible".to_string(),
            other => format!("tag=0x{other:04x}"),
        };
        let mut valid_bits = w_bits_per_sample;
        if format_tag == WAVE_FORMAT_EXTENSIBLE && cb_size >= 22 {
            let ext = core::ptr::read_unaligned(pwfx as *const WAVEFORMATEXTENSIBLE);
            let samples = ext.Samples;
            valid_bits = samples.wValidBitsPerSample;
            let sub = ext.SubFormat;
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                tag = "float".to_string();
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
                tag = "pcm".to_string();
            }
        }
        format!(
            "{n_samples_per_sec} Hz, {n_channels} ch, {w_bits_per_sample}-bit {tag} (valid_bits={valid_bits})"
        )
    }
}
