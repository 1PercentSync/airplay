//! WASAPI shared-mode loopback capture.
//! Sunshine flags minus AUTOCONVERTPCM-to-48k; we resample ourselves.
//! [evidence: Sunshine audio.cpp:390-398,668-808; docs/协议实现规范.md §12.1]

use crate::ring::SampleRing;
use airplay_core::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Process-wide 1ms timer (Sunshine `timeBeginPeriod` fallback). Windows default
/// ~15.6ms otherwise folds an 8ms RTP period into ~16ms.
struct Timer1ms;

impl Timer1ms {
    fn request() -> Self {
        #[cfg(windows)]
        unsafe {
            timeBeginPeriod(1);
        }
        Self
    }
}

impl Drop for Timer1ms {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            timeEndPeriod(1);
        }
    }
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(u_period: u32) -> u32;
    fn timeEndPeriod(u_period: u32) -> u32;
}

pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    ring: Arc<SampleRing>,
    _timer: Timer1ms,
}

impl Capture {
    pub fn start(device_hint: Option<&str>) -> Result<Self> {
        let ring = Arc::new(SampleRing::new());
        let stop = Arc::new(AtomicBool::new(false));
        let hint = device_hint.map(str::to_string);
        let (ready_tx, ready_rx) = mpsc::channel();
        let ring_t = ring.clone();
        let stop_t = stop.clone();
        let thread = thread::Builder::new()
            .name("wasapi-capture".into())
            .spawn(move || {
                #[cfg(windows)]
                {
                    if let Err(e) = windows_cap::run(hint.as_deref(), ring_t, stop_t, ready_tx) {
                        tracing::error!("WASAPI capture: {e}");
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = (hint, ring_t, stop_t);
                    let _ = ready_tx.send(Err(Error::Audio(
                        "WASAPI capture is Windows-only (build and run on Windows)".into(),
                    )));
                }
            })
            .map_err(|e| Error::Audio(format!("spawn capture: {e}")))?;
        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
                ring,
                _timer: Timer1ms::request(),
            }),
            Ok(Err(e)) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err(Error::Audio("capture start timed out".into()))
            }
        }
    }

    pub fn ring(&self) -> Arc<SampleRing> {
        self.ring.clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(windows)]
mod windows_cap {
    use super::*;
    use std::sync::mpsc::Sender;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    const FLOAT_GUID: u128 = 0x0000_0003_0000_0010_8000_00aa_0038_9b71;
    const PCM_GUID: u128 = 0x0000_0001_0000_0010_8000_00aa_0038_9b71;
    const SILENT: u32 = 2;
    const DISC: u32 = 1;

    #[link(name = "avrt")]
    extern "system" {
        fn AvSetMmThreadCharacteristicsW(name: *const u16, index: *mut u32) -> HANDLE;
        fn AvRevertMmThreadCharacteristics(handle: HANDLE) -> i32;
    }

    pub fn run(
        hint: Option<&str>,
        ring: Arc<SampleRing>,
        stop: Arc<AtomicBool>,
        ready: Sender<Result<()>>,
    ) -> Result<()> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| Error::Audio(format!("CoInitializeEx: {e}")))?;
        }
        let result = unsafe { run_com(hint, ring, stop, ready) };
        unsafe { CoUninitialize() };
        result
    }

    unsafe fn run_com(
        hint: Option<&str>,
        ring: Arc<SampleRing>,
        stop: Arc<AtomicBool>,
        ready: Sender<Result<()>>,
    ) -> Result<()> {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| Error::Audio(format!("MMDeviceEnumerator: {e}")))?;
        let device = pick_device(&enumerator, hint)?;
        let mut announced = false;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let notify = if announced { None } else { Some(&ready) };
            match session(&device, &ring, &stop, notify) {
                Ok(()) => {
                    announced = true;
                    if stop.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    tracing::warn!("WASAPI session ended, reopening");
                }
                Err(e) => {
                    if !announced {
                        let _ = ready.send(Err(Error::Audio(e.to_string())));
                        return Err(e);
                    }
                    tracing::error!("WASAPI reinit: {e}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    unsafe fn pick_device(
        enumerator: &IMMDeviceEnumerator,
        hint: Option<&str>,
    ) -> Result<IMMDevice> {
        if let Some(h) = hint {
            if let Ok(dev) = enumerator.GetDevice(&HSTRING::from(h)) {
                tracing::info!("capture device by id");
                return Ok(dev);
            }
            let collection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| Error::Audio(format!("EnumAudioEndpoints: {e}")))?;
            let count = collection
                .GetCount()
                .map_err(|e| Error::Audio(format!("GetCount: {e}")))?;
            let h_l = h.to_ascii_lowercase();
            for i in 0..count {
                let device = collection
                    .Item(i)
                    .map_err(|e| Error::Audio(format!("Item: {e}")))?;
                let name = crate::enum_devices::imm_friendly_name(&device);
                if name.to_ascii_lowercase().contains(&h_l) {
                    tracing::info!(name, "capture device by name");
                    return Ok(device);
                }
            }
            return Err(Error::Audio(format!("no render device matching {h}")));
        }
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::Audio(format!("EnumAudioEndpoints: {e}")))?;
        let count = collection
            .GetCount()
            .map_err(|e| Error::Audio(format!("GetCount: {e}")))?;
        for i in 0..count {
            let device = collection
                .Item(i)
                .map_err(|e| Error::Audio(format!("Item: {e}")))?;
            let name = crate::enum_devices::imm_friendly_name(&device);
            if name
                .to_ascii_lowercase()
                .contains("steam streaming speakers")
            {
                tracing::info!(name, "capture default Steam Streaming Speakers");
                return Ok(device);
            }
        }
        enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| Error::Audio(format!("GetDefaultAudioEndpoint: {e}")))
    }

    unsafe fn session(
        device: &IMMDevice,
        ring: &SampleRing,
        stop: &AtomicBool,
        ready: Option<&Sender<Result<()>>>,
    ) -> Result<()> {
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| Error::Audio(format!("Activate IAudioClient: {e}")))?;
        let fmt_ptr = client
            .GetMixFormat()
            .map_err(|e| Error::Audio(format!("GetMixFormat: {e}")))?;
        if fmt_ptr.is_null() {
            return Err(Error::Audio("GetMixFormat null".into()));
        }
        let fmt: WAVEFORMATEX = *fmt_ptr;
        let rate = fmt.nSamplesPerSec;
        let ch = fmt.nChannels;
        let bits = fmt.wBitsPerSample;
        let tag = fmt.wFormatTag;
        let mut subtype = match tag {
            0x0001 => "pcm",
            0x0003 => "float",
            0xFFFE => "extensible",
            _ => "other",
        };
        let mut valid = bits;
        if tag == 0xFFFE {
            let ext = &*(fmt_ptr as *const WAVEFORMATEXTENSIBLE);
            valid = ext.Samples.wValidBitsPerSample;
            let g = ext.SubFormat;
            subtype = if g == windows::core::GUID::from_u128(FLOAT_GUID) {
                "float"
            } else if g == windows::core::GUID::from_u128(PCM_GUID) {
                "pcm"
            } else {
                "extensible-other"
            };
        }
        tracing::info!(rate, ch, bits, valid, subtype, "loopback mix format");
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                0,
                0,
                fmt_ptr,
                None,
            )
            .map_err(|e| Error::Audio(format!("Initialize: {e}")))?;
        CoTaskMemFree(Some(fmt_ptr as *const core::ffi::c_void as *mut _));

        let event = CreateEventW(None, false, false, None)
            .map_err(|e| Error::Audio(format!("CreateEventW: {e}")))?;
        client
            .SetEventHandle(event)
            .map_err(|e| Error::Audio(format!("SetEventHandle: {e}")))?;
        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| Error::Audio(format!("IAudioCaptureClient: {e}")))?;

        let mut mmcss_idx = 0u32;
        let pro: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mmcss = AvSetMmThreadCharacteristicsW(pro.as_ptr(), &mut mmcss_idx);

        ring.set_format(rate);
        client
            .Start()
            .map_err(|e| Error::Audio(format!("Start: {e}")))?;
        if let Some(r) = ready {
            let _ = r.send(Ok(()));
        }

        // Sndvol device slider vs loopback tap.
        // No hardware volume → Windows substitutes software volume in the engine
        // [evidence: ms_query_hardware_support.md]. No hardware loopback pin →
        // WASAPI copies that engine output to the render pin and to us
        // [evidence: ms_loopback_recording.md]. Multiplying again would square it.
        // Hardware volume → DAC after the tap; then we do apply the scalar.
        let epvol: Option<IAudioEndpointVolume> = device.Activate(CLSCTX_ALL, None).ok();
        let apply_scalar = match &epvol {
            Some(ep) => match ep.QueryHardwareSupport() {
                Ok(mask) => {
                    let hw_vol = (mask & 0x1) != 0;
                    tracing::info!(
                        mask,
                        hw_vol,
                        "QueryHardwareSupport; multiply PCM only if hardware volume"
                    );
                    hw_vol
                }
                Err(e) => {
                    tracing::info!(
                        error = %e,
                        "QueryHardwareSupport failed, not multiplying PCM"
                    );
                    false
                }
            }
            None => false,
        };
        if apply_scalar {
            tracing::info!("endpoint volume is hardware, applying as capture gain");
        }
        let run = capture_loop(
            &capture,
            event,
            ring,
            stop,
            rate,
            ch,
            bits,
            subtype,
            if apply_scalar { epvol.as_ref() } else { None },
        );
        let _ = client.Stop();
        if !mmcss.is_invalid() {
            AvRevertMmThreadCharacteristics(mmcss);
        }
        windows::Win32::Foundation::CloseHandle(event).ok();
        run
    }

    unsafe fn capture_loop(
        capture: &IAudioCaptureClient,
        event: HANDLE,
        ring: &SampleRing,
        stop: &AtomicBool,
        rate: u32,
        ch: u16,
        bits: u16,
        subtype: &str,
        epvol: Option<&IAudioEndpointVolume>,
    ) -> Result<()> {
        let _ = rate;
        while !stop.load(Ordering::SeqCst) {
            poll_endpoint_gain(epvol, ring);
            let wr = WaitForSingleObject(event, 2000);
            if wr == WAIT_TIMEOUT {
                continue;
            }
            if wr != WAIT_OBJECT_0 {
                return Err(Error::Audio(format!("WaitForSingleObject {wr:?}")));
            }
            loop {
                let pkt = match capture.GetNextPacketSize() {
                    Ok(n) => n,
                    Err(e) => {
                        return Err(Error::Audio(format!("GetNextPacketSize: {e}")));
                    }
                };
                if pkt == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| Error::Audio(format!("GetBuffer: {e}")))?;
                if flags & DISC != 0 {
                    ring.disc.fetch_add(1, Ordering::Relaxed);
                }
                if frames > 0 && !data.is_null() {
                    let stereo = if flags & SILENT != 0 {
                        vec![0.0f32; frames as usize * 2]
                    } else {
                        convert_to_stereo(data, frames as usize, ch, bits, subtype)
                    };
                    ring.push_stereo(&stereo);
                }
                capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| Error::Audio(format!("ReleaseBuffer: {e}")))?;
            }
        }
        Ok(())
    }

    fn poll_endpoint_gain(epvol: Option<&IAudioEndpointVolume>, ring: &SampleRing) {
        let Some(ep) = epvol else {
            return;
        };
        unsafe {
            let scalar = match ep.GetMasterVolumeLevelScalar() {
                Ok(v) => v,
                Err(_) => return,
            };
            let muted = ep.GetMute().map(|b| b.as_bool()).unwrap_or(false);
            let g = if muted { 0.0 } else { scalar };
            let prev = ring.endpoint_gain();
            ring.set_endpoint_gain(g);
            if (prev - g).abs() > 0.01 {
                tracing::info!(g, muted, "endpoint gain applied to capture PCM");
            }
        }
    }

    unsafe fn convert_to_stereo(
        data: *mut u8,
        frames: usize,
        ch: u16,
        bits: u16,
        subtype: &str,
    ) -> Vec<f32> {
        let ch = ch.max(1) as usize;
        let mut out = vec![0.0f32; frames * 2];
        match (subtype, bits) {
            ("float", 32) => {
                let s = std::slice::from_raw_parts(data as *const f32, frames * ch);
                for i in 0..frames {
                    out[i * 2] = s[i * ch];
                    out[i * 2 + 1] = if ch > 1 { s[i * ch + 1] } else { s[i * ch] };
                }
            }
            ("pcm", 16) => {
                let s = std::slice::from_raw_parts(data as *const i16, frames * ch);
                for i in 0..frames {
                    out[i * 2] = s[i * ch] as f32 / 32768.0;
                    let r = if ch > 1 { s[i * ch + 1] } else { s[i * ch] };
                    out[i * 2 + 1] = r as f32 / 32768.0;
                }
            }
            ("pcm", 32) => {
                let s = std::slice::from_raw_parts(data as *const i32, frames * ch);
                for i in 0..frames {
                    out[i * 2] = s[i * ch] as f32 / 2147483648.0;
                    let r = if ch > 1 { s[i * ch + 1] } else { s[i * ch] };
                    out[i * 2 + 1] = r as f32 / 2147483648.0;
                }
            }
            _ => {
                // treat as 32-bit float mix (Windows default)
                let s = std::slice::from_raw_parts(data as *const f32, frames * ch);
                for i in 0..frames {
                    out[i * 2] = s[i * ch];
                    out[i * 2 + 1] = if ch > 1 { s[i * ch + 1] } else { s[i * ch] };
                }
            }
        }
        out
    }
}
