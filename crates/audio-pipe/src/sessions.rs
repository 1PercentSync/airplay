//! Active-session check: is Edge/Chrome rendering on this endpoint?
//!
//! Per-endpoint session enumeration (IAudioSessionManager2) is the public
//! way to see which process feeds a render device. Chromium audio usually
//! lives in a utility process (`--utility-sub-type=audio.mojom.AudioService`)
//! but its executable is still msedge.exe / chrome.exe, so checking the
//! process image name covers both cases. Process granularity only: sessions
//! cannot tell one tab from another.

use airplay_core::Result;

/// True when an active audio session on `device_id` belongs to Edge/Chrome.
#[cfg(windows)]
pub fn browser_active_on(device_id: &str) -> Result<bool> {
    windows_sessions::browser_active_on(device_id)
}

/// Non-Windows builds never see a browser (the API runs on Windows only).
#[cfg(not(windows))]
pub fn browser_active_on(_device_id: &str) -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
mod windows_sessions {
    use airplay_core::{Error, Result};
    use windows::core::Interface;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const BROWSERS: [&str; 2] = ["msedge.exe", "chrome.exe"];

    pub fn browser_active_on(device_id: &str) -> Result<bool> {
        unsafe { inner(device_id) }
    }

    unsafe fn inner(device_id: &str) -> Result<bool> {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| Error::Audio(format!("CoInitializeEx: {e}")))?;
        let result = (|| {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| Error::Audio(format!("MMDeviceEnumerator: {e}")))?;
            let collection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| Error::Audio(format!("EnumAudioEndpoints: {e}")))?;
            let count = collection
                .GetCount()
                .map_err(|e| Error::Audio(format!("GetCount: {e}")))?;
            for i in 0..count {
                let device = collection
                    .Item(i)
                    .map_err(|e| Error::Audio(format!("Item {i}: {e}")))?;
                let id_pwstr = device
                    .GetId()
                    .map_err(|e| Error::Audio(format!("GetId: {e}")))?;
                let id = id_pwstr.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id_pwstr.0 as *const core::ffi::c_void as *mut _));
                if id == device_id {
                    return endpoint_has_browser(&device);
                }
            }
            Err(Error::Audio(format!("no active endpoint {device_id}")))
        })();
        CoUninitialize();
        result
    }

    unsafe fn endpoint_has_browser(device: &IMMDevice) -> Result<bool> {
        let manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| Error::Audio(format!("Activate IAudioSessionManager2: {e}")))?;
        let sessions = manager
            .GetSessionEnumerator()
            .map_err(|e| Error::Audio(format!("GetSessionEnumerator: {e}")))?;
        let count = sessions
            .GetCount()
            .map_err(|e| Error::Audio(format!("session GetCount: {e}")))?;
        for i in 0..count {
            let Ok(control) = sessions.GetSession(i) else {
                continue;
            };
            let Ok(state) = control.GetState() else {
                continue;
            };
            if state != AudioSessionStateActive {
                continue;
            }
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let Ok(pid) = control2.GetProcessId() else {
                continue;
            };
            if is_browser_process(pid) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    unsafe fn is_browser_process(pid: u32) -> bool {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let name = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()
        .map(|()| {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().unwrap_or("").to_ascii_lowercase()
        });
        let _ = CloseHandle(handle);
        matches!(name.as_deref(), Some(n) if BROWSERS.contains(&n))
    }
}
