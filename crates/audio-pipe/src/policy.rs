//! Set the system default render endpoint via the undocumented
//! IPolicyConfig COM interface — the same approach as Sunshine.
//!
//! [evidence: Sunshine src/platform/windows/PolicyConfig.h — CLSID/IID and
//! vtable order; Sunshine src/platform/windows/audio.cpp:1041 —
//! SetDefaultEndpoint called for every ERole (console/multimedia/comms)]

use airplay_core::Result;

/// Make `device_id` the default render endpoint for all roles.
#[cfg(windows)]
pub fn set_default_render_device(device_id: &str) -> Result<()> {
    windows_policy::set_default_render_device(device_id)
}

#[cfg(not(windows))]
pub fn set_default_render_device(_device_id: &str) -> Result<()> {
    Err(airplay_core::Error::Audio(
        "IPolicyConfig is Windows-only (build and run on Windows)".into(),
    ))
}

#[cfg(windows)]
mod windows_policy {
    use airplay_core::{Error, Result};
    use core::ffi::c_void;
    use windows::core::{GUID, HRESULT, PCWSTR};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, CLSCTX, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    // Raw CoCreateInstance: the windows-crate generic is keyed on T::IID,
    // which cannot express this undocumented interface. Sunshine creates
    // the object with IID_IPolicyConfig explicitly; do the same.
    windows::core::link!("ole32.dll" "system" fn CoCreateInstance(rclsid: *const GUID, punkouter: *mut c_void, dwclscontext: CLSCTX, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT);

    /// CPolicyConfigClient {870AF99C-171D-4F9E-AF0D-E63DF40C2BC9}
    /// [evidence: Sunshine PolicyConfig.h]
    const CLSID_CPOLICYCONFIGCLIENT: GUID =
        GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
    /// IPolicyConfig {F8679F50-850A-41CF-9C72-430F290290C8}
    /// [evidence: Sunshine PolicyConfig.h]
    const IID_IPOLICYCONFIG: GUID =
        GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

    /// IPolicyConfig vtable, slots in declaration order from Sunshine's
    /// PolicyConfig.h. Release and SetDefaultEndpoint are called; the rest
    /// are placeholders to keep the layout intact.
    #[repr(C)]
    struct IPolicyConfigVtbl {
        // IUnknown
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        // IPolicyConfig
        get_mix_format: usize,
        get_device_format: usize,
        reset_device_format: usize,
        set_device_format: usize,
        get_processing_period: usize,
        set_processing_period: usize,
        get_share_mode: usize,
        set_share_mode: usize,
        get_property_value: usize,
        set_property_value: usize,
        set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT,
        set_endpoint_visibility: usize,
    }

    pub fn set_default_render_device(device_id: &str) -> Result<()> {
        unsafe { inner(device_id) }
    }

    unsafe fn inner(device_id: &str) -> Result<()> {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| Error::Audio(format!("CoInitializeEx: {e}")))?;
        let result = (|| {
            let mut this: *mut c_void = std::ptr::null_mut();
            CoCreateInstance(
                &CLSID_CPOLICYCONFIGCLIENT,
                std::ptr::null_mut(),
                CLSCTX_ALL,
                &IID_IPOLICYCONFIG,
                &mut this,
            )
            .ok()
            .map_err(|e| Error::Audio(format!("CPolicyConfigClient: {e}")))?;
            if this.is_null() {
                return Err(Error::Audio("CPolicyConfigClient returned null".into()));
            }
            let vtbl: &IPolicyConfigVtbl = &**(this as *const *const IPolicyConfigVtbl);
            let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
            let mut result = Ok(());
            // eConsole, eMultimedia, eCommunications — Sunshine sets all roles.
            for role in 0..3 {
                if let Err(e) =
                    (vtbl.set_default_endpoint)(this, PCWSTR(wide.as_ptr()), role).ok()
                {
                    result = Err(Error::Audio(format!("SetDefaultEndpoint role {role}: {e}")));
                    break;
                }
            }
            (vtbl.release)(this);
            result
        })();
        CoUninitialize();
        result
    }
}
