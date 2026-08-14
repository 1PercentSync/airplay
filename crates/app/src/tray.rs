//! Windows tray: device pick, start/stop, HomePod volume, status tooltip.
//! Menu pattern follows Sunshine system_tray (Open/Quit style), without its web UI.
//! [evidence: Sunshine src/system_tray.h; windows-rs Shell_NotifyIconW]

#![cfg(windows)]

use crate::autostart;
use crate::config::Config;
use crate::probe::{self, Discovered};
use crate::run::{self, SessionCtrl};
use crate::sunshine;
use airplay_stream::{latency_preset_label, LATENCY_PRESETS};
use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, VK_H,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, LoadIconW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_COMMAND, WM_DESTROY, WM_HOTKEY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

const WM_TRAY: u32 = WM_APP + 1;
const ID_START: u16 = 1001;
const ID_STOP: u16 = 1002;
const ID_REFRESH: u16 = 1003;
const ID_QUIT: u16 = 1004;
const ID_SUNSHINE: u16 = 1005;
const ID_AUTOSTART: u16 = 1006;
const ID_VOL_BASE: u16 = 1100;
const ID_DEV_BASE: u16 = 1200;
const ID_CAP_BASE: u16 = 1300;
const ID_LAT_BASE: u16 = 1400;
/// Ctrl+Alt+H toggles HomePod mode (default device switch + stream).
const HOTKEY_ID: i32 = 0xA1;

enum Cmd {
    Start,
    Stop,
    SetVolume(f64),
    SelectTarget { target: String, name: String },
    SelectCapture(String),
    SetSunshineAware(bool),
    SetAutostart(bool),
    SetLatency(u32),
    ToggleHomePod,
    Refresh,
    Quit,
}

struct Shared {
    devices: Mutex<Vec<Discovered>>,
    captures: Mutex<Vec<(String, String)>>,
    target: Mutex<String>,
    device_name: Mutex<String>,
    capture: Mutex<String>,
    volume: Mutex<f64>,
    play_wanted: AtomicBool,
    sunshine_aware: AtomicBool,
    autostart: AtomicBool,
    latency_frames: Mutex<u32>,
    api: Arc<crate::api::ApiState>,
    api_port: u16,
    /// Default render device id remembered on entering HomePod mode.
    prev_default: Mutex<String>,
    running: AtomicBool,
    tooltip: Mutex<String>,
}

pub fn run() -> Result<()> {
    let cfg = Config::load();
    let shared = Arc::new(Shared {
        devices: Mutex::new(Vec::new()),
        captures: Mutex::new(Vec::new()),
        target: Mutex::new(cfg.device_ip.clone()),
        device_name: Mutex::new(cfg.device_name.clone()),
        capture: Mutex::new(cfg.capture_device.clone()),
        volume: Mutex::new(cfg.volume.clamp(0.0, 1.0)),
        play_wanted: AtomicBool::new(cfg.play),
        sunshine_aware: AtomicBool::new(cfg.sunshine_aware),
        autostart: AtomicBool::new(cfg.autostart),
        latency_frames: Mutex::new(cfg.latency_frames),
        api: Arc::new(crate::api::ApiState::new(
            cfg.capture_device.clone(),
            cfg.latency_frames,
        )),
        api_port: cfg.api_port,
        prev_default: Mutex::new(cfg.hotkey_previous_device.clone()),
        running: AtomicBool::new(false),
        tooltip: Mutex::new("airplay: idle".into()),
    });
    refresh_captures(&shared);
    if let Err(e) = autostart::apply(cfg.autostart) {
        warn!("apply autostart: {e}");
    }

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let cmd_quit = cmd_tx.clone();
    let rt = tokio::runtime::Runtime::new()?;
    let handle = rt.handle().clone();
    crate::api::spawn(shared.api.clone(), cfg.api_port, &handle);
    let shared_rt = shared.clone();
    let join = std::thread::Builder::new()
        .name("airplay-tokio".into())
        .spawn(move || {
            rt.block_on(worker(shared_rt, cmd_rx));
        })
        .expect("spawn tokio");

    let _ = handle.spawn({
        let tx = cmd_tx.clone();
        async move {
            let _ = tx.send(Cmd::Refresh);
        }
    });

    let result = unsafe { message_loop(shared, cmd_tx, handle) };
    let _ = cmd_quit.send(Cmd::Quit);
    let _ = join.join();
    result
}

fn refresh_captures(shared: &Shared) {
    match audio_pipe::list_render_devices() {
        Ok(list) => {
            let rows: Vec<(String, String)> = list
                .into_iter()
                .map(|d| (d.friendly_name, d.id))
                .collect();
            *shared.captures.lock().unwrap() = rows;
        }
        Err(e) => warn!("list capture devices: {e}"),
    }
}

async fn worker(shared: Arc<Shared>, mut cmd_rx: mpsc::UnboundedReceiver<Cmd>) {
    let slot: Arc<Mutex<Option<SessionCtrl>>> = Arc::new(Mutex::new(None));
    let reconnect = Arc::new(AtomicU64::new(0));
    let mut session_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut hang = false;
    let mut sun_on = sunshine::app_connected().await;
    let mut booted = false;
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                match cmd {
                    Cmd::Refresh => {
                        refresh_captures(&shared);
                        match probe::browse().await {
                            Ok(list) => {
                                let want_name = shared.device_name.lock().unwrap().clone();
                                if shared.target.lock().unwrap().is_empty() {
                                    let pick = list
                                        .iter()
                                        .find(|d| !want_name.is_empty() && d.display_name() == want_name)
                                        .or_else(|| list.first());
                                    if let Some(d) = pick {
                                        if let Some(t) = d.target() {
                                            *shared.target.lock().unwrap() = t;
                                            *shared.device_name.lock().unwrap() = d.display_name();
                                            save_cfg(&shared);
                                        }
                                    }
                                }
                                *shared.devices.lock().unwrap() = list;
                            }
                            Err(e) => warn!("browse: {e}"),
                        }
                        if !booted {
                            booted = true;
                            sun_on = sunshine::app_connected().await;
                            if shared.play_wanted.load(Ordering::SeqCst) {
                                let aware = shared.sunshine_aware.load(Ordering::SeqCst);
                                if aware && sun_on {
                                    hang = true;
                                    *shared.tooltip.lock().unwrap() =
                                        "airplay: waiting (Sunshine)".into();
                                    info!("play remembered on; Sunshine already connected, deferring");
                                } else {
                                    begin_session(&shared, &slot, &reconnect, &mut session_task).await;
                                }
                            }
                        }
                    }
                    Cmd::SelectTarget { target, name } => {
                        *shared.target.lock().unwrap() = target.clone();
                        *shared.device_name.lock().unwrap() = name;
                        save_cfg(&shared);
                        info!(target = %target, "selected HomePod");
                    }
                    Cmd::SelectCapture(name) => {
                        *shared.capture.lock().unwrap() = name.clone();
                        shared.api.set_capture_hint(name.clone());
                        save_cfg(&shared);
                        info!(name, "selected capture");
                    }
                    Cmd::SetVolume(v) => {
                        *shared.volume.lock().unwrap() = v;
                        save_cfg(&shared);
                        if let Some(s) = slot.lock().unwrap().as_ref() {
                            s.set_volume(v);
                        }
                    }
                    Cmd::SetSunshineAware(on) => {
                        shared.sunshine_aware.store(on, Ordering::SeqCst);
                        if !on {
                            hang = false;
                        }
                        save_cfg(&shared);
                        info!(on, "Sunshine aware");
                    }
                    Cmd::SetAutostart(on) => {
                        shared.autostart.store(on, Ordering::SeqCst);
                        save_cfg(&shared);
                        if let Err(e) = autostart::apply(on) {
                            warn!("autostart: {e}");
                        } else {
                            info!(on, "Start with Windows");
                        }
                    }
                    Cmd::ToggleHomePod => {
                        let hint = shared.capture.lock().unwrap().clone();
                        let hint = if hint.is_empty() {
                            None
                        } else {
                            Some(hint.as_str())
                        };
                        // Same device resolution as capture; compare with the
                        // live default to decide the direction.
                        let resolved = audio_pipe::pick_render_device_id(hint)
                            .and_then(|cid| {
                                audio_pipe::default_render_device_id().map(|cur| (cid, cur))
                            });
                        match &resolved {
                            Ok((capture_id, cur)) => {
                                info!(capture = %capture_id, current = %cur, "hotkey toggle");
                            }
                            Err(_) => {}
                        }
                        match resolved {
                            Ok((capture_id, cur)) if cur == capture_id => {
                                // Leaving HomePod mode: restore the remembered
                                // default device, then stop streaming.
                                let prev =
                                    std::mem::take(&mut *shared.prev_default.lock().unwrap());
                                if !prev.is_empty() {
                                    if let Err(e) = audio_pipe::set_default_render_device(&prev) {
                                        warn!("restore default device: {e}");
                                    }
                                }
                                shared.play_wanted.store(false, Ordering::SeqCst);
                                hang = false;
                                save_cfg(&shared);
                                stop_session(&shared, &slot, &mut session_task).await;
                                info!("hotkey: left HomePod mode");
                            }
                            Ok((capture_id, cur)) => {
                                // Entering HomePod mode: remember the current
                                // default, switch to the capture endpoint, start.
                                if let Err(e) = audio_pipe::set_default_render_device(&capture_id)
                                {
                                    // Audio would never reach the capture
                                    // endpoint; do not start a silent stream.
                                    warn!("switch default device: {e}");
                                } else {
                                    *shared.prev_default.lock().unwrap() = cur;
                                    shared.play_wanted.store(true, Ordering::SeqCst);
                                    hang = false;
                                    save_cfg(&shared);
                                    begin_session(&shared, &slot, &reconnect, &mut session_task)
                                        .await;
                                    info!("hotkey: entered HomePod mode");
                                }
                            }
                            Err(e) => warn!("hotkey: resolve devices: {e}"),
                        }
                    }
                    Cmd::SetLatency(frames) => {
                        let prev = *shared.latency_frames.lock().unwrap();
                        *shared.latency_frames.lock().unwrap() = frames;
                        shared.api.set_lead_frames(frames);
                        save_cfg(&shared);
                        info!(frames, label = %latency_preset_label(frames), "latency preset");
                        let playing = slot.lock().unwrap().is_some() || session_task.is_some();
                        if playing && frames != prev && !hang {
                            stop_session(&shared, &slot, &mut session_task).await;
                            begin_session(&shared, &slot, &reconnect, &mut session_task).await;
                        }
                    }
                    Cmd::Start => {
                        shared.play_wanted.store(true, Ordering::SeqCst);
                        hang = false;
                        save_cfg(&shared);
                        begin_session(&shared, &slot, &reconnect, &mut session_task).await;
                    }
                    Cmd::Stop => {
                        shared.play_wanted.store(false, Ordering::SeqCst);
                        hang = false;
                        save_cfg(&shared);
                        stop_session(&shared, &slot, &mut session_task).await;
                    }
                    Cmd::Quit => {
                        stop_session(&shared, &slot, &mut session_task).await;
                        break;
                    }
                }
            }
            _ = tick.tick() => {
                let now = sunshine::app_connected().await;
                if now == sun_on {
                    continue;
                }
                let was = sun_on;
                sun_on = now;
                if !shared.sunshine_aware.load(Ordering::SeqCst) {
                    continue;
                }
                let playing = slot.lock().unwrap().is_some();
                if !was && now && playing {
                    info!("Sunshine connected, pausing HomePod once");
                    hang = true;
                    stop_session(&shared, &slot, &mut session_task).await;
                    *shared.tooltip.lock().unwrap() = "airplay: waiting (Sunshine)".into();
                } else if was && !now && hang {
                    info!("Sunshine disconnected, resuming HomePod");
                    hang = false;
                    begin_session(&shared, &slot, &reconnect, &mut session_task).await;
                }
            }
        }
    }
}

async fn begin_session(
    shared: &Arc<Shared>,
    slot: &Arc<Mutex<Option<SessionCtrl>>>,
    reconnect: &Arc<AtomicU64>,
    session_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(h) = session_task.take() {
        if !h.is_finished() {
            *session_task = Some(h);
            return;
        }
        let _ = h.await;
    }
    if slot.lock().unwrap().is_some() {
        return;
    }
    let target = shared.target.lock().unwrap().clone();
    if target.is_empty() {
        warn!("no HomePod selected");
        *shared.tooltip.lock().unwrap() = "airplay: no device".into();
        return;
    }
    let capture = shared.capture.lock().unwrap().clone();
    let hint = if capture.is_empty() {
        None
    } else {
        Some(capture)
    };
    let vol = *shared.volume.lock().unwrap();
    let (ctrl, stop_rx, vol_rx) = SessionCtrl::new(vol);
    *slot.lock().unwrap() = Some(ctrl);
    shared.running.store(true, Ordering::SeqCst);
    *shared.tooltip.lock().unwrap() = format!("airplay: connecting {target}");
    let sh = shared.clone();
    let rec = reconnect.clone();
    let slot_t = slot.clone();
    *session_task = Some(tokio::spawn(async move {
        let status = {
            let sh = sh.clone();
            Arc::new(move |s: &str| {
                info!("{s}");
                sh.api.set_streaming(s == "[STATUS] streaming");
                let line = s.trim_start_matches("[STATUS] ").to_string();
                *sh.tooltip.lock().unwrap() = format!("airplay: {line}");
            }) as Arc<dyn Fn(&str) + Send + Sync>
        };
        status("[STATUS] connecting");
        let latency_frames = *sh.latency_frames.lock().unwrap();
        if let Err(e) =
            run::run_supervised(target, hint, stop_rx, vol_rx, rec, status, latency_frames).await
        {
            error!("supervised session: {e}");
        }
        sh.api.set_streaming(false);
        *slot_t.lock().unwrap() = None;
        sh.running.store(false, Ordering::SeqCst);
        if sh.tooltip.lock().unwrap().starts_with("airplay: waiting") {
            return;
        }
        *sh.tooltip.lock().unwrap() = "airplay: idle".into();
    }));
}

async fn stop_session(
    shared: &Shared,
    slot: &Arc<Mutex<Option<SessionCtrl>>>,
    session_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(s) = slot.lock().unwrap().take() {
        s.request_stop();
    }
    shared.running.store(false, Ordering::SeqCst);
    if let Some(h) = session_task.take() {
        let _ = h.await;
    }
    if shared.tooltip.lock().unwrap().starts_with("airplay: waiting") {
        return;
    }
    *shared.tooltip.lock().unwrap() = "airplay: idle".into();
}

fn save_cfg(shared: &Shared) {
    let cfg = Config {
        device_ip: shared.target.lock().unwrap().clone(),
        device_name: shared.device_name.lock().unwrap().clone(),
        capture_device: shared.capture.lock().unwrap().clone(),
        volume: *shared.volume.lock().unwrap(),
        play: shared.play_wanted.load(Ordering::SeqCst),
        sunshine_aware: shared.sunshine_aware.load(Ordering::SeqCst),
        autostart: shared.autostart.load(Ordering::SeqCst),
        latency_frames: *shared.latency_frames.lock().unwrap(),
        api_port: shared.api_port,
        hotkey_previous_device: shared.prev_default.lock().unwrap().clone(),
    };
    if let Err(e) = cfg.save() {
        warn!("save config: {e}");
    }
}

unsafe fn message_loop(
    shared: Arc<Shared>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    _handle: tokio::runtime::Handle,
) -> Result<()> {
    let class = w!("airplay_tray");
    let instance = GetModuleHandleW(None)?;
    let hinstance = HINSTANCE(instance.0);
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        w!("airplay"),
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        None,
        None,
        Some(hinstance),
        None,
    )?;

    let boxed = Box::new((shared.clone(), cmd_tx.clone()));
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(boxed) as isize);

    let icon = LoadIconW(Some(hinstance), PCWSTR(2usize as *const u16))?;
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    write_tip(&mut nid, "airplay: idle");
    Shell_NotifyIconW(NIM_ADD, &nid).ok()?;

    if let Err(e) = RegisterHotKey(Some(hwnd), HOTKEY_ID, MOD_CONTROL | MOD_ALT, VK_H.0 as u32) {
        warn!("register Ctrl+Alt+H hotkey: {e}");
    }

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let tip = shared.tooltip.lock().unwrap().clone();
        write_tip(&mut nid, &tip);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    let _ = DestroyWindow(hwnd);
    Ok(())
}

fn write_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let encoded: Vec<u16> = text.encode_utf16().take(127).collect();
    nid.szTip = [0; 128];
    nid.szTip[..encoded.len()].copy_from_slice(&encoded);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if ptr != 0 {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(
                ptr as *mut (Arc<Shared>, mpsc::UnboundedSender<Cmd>),
            ));
        }
        PostQuitMessage(0);
        return LRESULT(0);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if ptr == 0 {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let pair = &*(ptr as *const (Arc<Shared>, mpsc::UnboundedSender<Cmd>));
    let (shared, cmd_tx) = pair;

    if msg == WM_TRAY && (lparam.0 as u32 == WM_RBUTTONUP || lparam.0 as u32 == WM_LBUTTONUP) {
        show_menu(hwnd, shared, cmd_tx);
        return LRESULT(0);
    }
    if msg == WM_HOTKEY && wparam.0 as i32 == HOTKEY_ID {
        let _ = cmd_tx.send(Cmd::ToggleHomePod);
        return LRESULT(0);
    }
    if msg == WM_COMMAND {
        handle_command(wparam.0 as u16, shared, cmd_tx);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn show_menu(hwnd: HWND, shared: &Shared, _cmd_tx: &mpsc::UnboundedSender<Cmd>) {
    let menu = CreatePopupMenu().unwrap();
    let running = shared.running.load(Ordering::SeqCst);
    let status = shared.tooltip.lock().unwrap().clone();
    let st: Vec<u16> = encode(&status);
    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(st.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

    let start: Vec<u16> = encode("Start");
    let stop: Vec<u16> = encode("Stop");
    if running {
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, ID_START as usize, PCWSTR(start.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_STOP as usize, PCWSTR(stop.as_ptr()));
    } else {
        let _ = AppendMenuW(menu, MF_STRING, ID_START as usize, PCWSTR(start.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, ID_STOP as usize, PCWSTR(stop.as_ptr()));
    }

    let sun: Vec<u16> = encode("Sunshine aware");
    let mut sun_flags = MF_STRING;
    if shared.sunshine_aware.load(Ordering::SeqCst) {
        sun_flags |= MF_CHECKED;
    }
    let _ = AppendMenuW(menu, sun_flags, ID_SUNSHINE as usize, PCWSTR(sun.as_ptr()));

    let boot: Vec<u16> = encode("Start with Windows");
    let mut boot_flags = MF_STRING;
    if shared.autostart.load(Ordering::SeqCst) {
        boot_flags |= MF_CHECKED;
    }
    let _ = AppendMenuW(menu, boot_flags, ID_AUTOSTART as usize, PCWSTR(boot.as_ptr()));

    let homepod = CreatePopupMenu().unwrap();
    let devices = shared.devices.lock().unwrap().clone();
    let cur = shared.target.lock().unwrap().clone();
    if devices.is_empty() {
        let none: Vec<u16> = encode("(none found)");
        let _ = AppendMenuW(homepod, MF_STRING | MF_GRAYED, 0, PCWSTR(none.as_ptr()));
    }
    for (i, d) in devices.iter().enumerate().take(20) {
        let t = d.target().unwrap_or_default();
        let label = format!("{} ({t})", d.display_name());
        let enc: Vec<u16> = encode(&label);
        let mut flags = MF_STRING;
        if t == cur {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(homepod, flags, (ID_DEV_BASE as usize) + i, PCWSTR(enc.as_ptr()));
    }
    let hp: Vec<u16> = encode("HomePod");
    let _ = AppendMenuW(menu, MF_POPUP, homepod.0 as usize, PCWSTR(hp.as_ptr()));

    let cap_menu = CreatePopupMenu().unwrap();
    let caps = shared.captures.lock().unwrap().clone();
    let cur_cap = shared.capture.lock().unwrap().clone();
    for (i, (name, _)) in caps.iter().enumerate().take(20) {
        let enc: Vec<u16> = encode(name);
        let mut flags = MF_STRING;
        if *name == cur_cap || (cur_cap.is_empty() && name.to_ascii_lowercase().contains("steam streaming"))
        {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(cap_menu, flags, (ID_CAP_BASE as usize) + i, PCWSTR(enc.as_ptr()));
    }
    let cap_l: Vec<u16> = encode("Capture");
    let _ = AppendMenuW(menu, MF_POPUP, cap_menu.0 as usize, PCWSTR(cap_l.as_ptr()));

    let vol_menu = CreatePopupMenu().unwrap();
    let vol = *shared.volume.lock().unwrap();
    for step in 0..=10u16 {
        let pct = step * 10;
        let enc: Vec<u16> = encode(&format!("{pct}%"));
        let mut flags = MF_STRING;
        if (vol * 10.0).round() as u16 == step {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(vol_menu, flags, (ID_VOL_BASE + step) as usize, PCWSTR(enc.as_ptr()));
    }
    let vol_l: Vec<u16> = encode("HomePod volume");
    let _ = AppendMenuW(menu, MF_POPUP, vol_menu.0 as usize, PCWSTR(vol_l.as_ptr()));

    let lat_menu = CreatePopupMenu().unwrap();
    let cur_lat = *shared.latency_frames.lock().unwrap();
    for (i, frames) in LATENCY_PRESETS.iter().enumerate() {
        let enc: Vec<u16> = encode(&latency_preset_label(*frames));
        let mut flags = MF_STRING;
        if *frames == cur_lat {
            flags |= MF_CHECKED;
        }
        let _ = AppendMenuW(
            lat_menu,
            flags,
            (ID_LAT_BASE as usize) + i,
            PCWSTR(enc.as_ptr()),
        );
    }
    let lat_l: Vec<u16> = encode("Latency");
    let _ = AppendMenuW(menu, MF_POPUP, lat_menu.0 as usize, PCWSTR(lat_l.as_ptr()));

    let ref_s: Vec<u16> = encode("Refresh devices");
    let quit: Vec<u16> = encode("Quit");
    let _ = AppendMenuW(menu, MF_STRING, ID_REFRESH as usize, PCWSTR(ref_s.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, PCWSTR(quit.as_ptr()));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
    let _ = DestroyMenu(menu);
}

fn handle_command(id: u16, shared: &Shared, cmd_tx: &mpsc::UnboundedSender<Cmd>) {
    match id {
        ID_START => {
            let _ = cmd_tx.send(Cmd::Start);
        }
        ID_STOP => {
            let _ = cmd_tx.send(Cmd::Stop);
        }
        ID_SUNSHINE => {
            let on = !shared.sunshine_aware.load(Ordering::SeqCst);
            let _ = cmd_tx.send(Cmd::SetSunshineAware(on));
        }
        ID_AUTOSTART => {
            let on = !shared.autostart.load(Ordering::SeqCst);
            let _ = cmd_tx.send(Cmd::SetAutostart(on));
        }
        ID_REFRESH => {
            let _ = cmd_tx.send(Cmd::Refresh);
        }
        ID_QUIT => {
            let _ = cmd_tx.send(Cmd::Quit);
            unsafe { PostQuitMessage(0) };
        }
        id if (ID_VOL_BASE..=ID_VOL_BASE + 10).contains(&id) => {
            let step = id - ID_VOL_BASE;
            let _ = cmd_tx.send(Cmd::SetVolume(f64::from(step) / 10.0));
        }
        id if (ID_DEV_BASE..ID_DEV_BASE + 20).contains(&id) => {
            let i = (id - ID_DEV_BASE) as usize;
            if let Some(d) = shared.devices.lock().unwrap().get(i).cloned() {
                if let Some(t) = d.target() {
                    let _ = cmd_tx.send(Cmd::SelectTarget {
                        target: t,
                        name: d.display_name(),
                    });
                }
            }
        }
        id if (ID_CAP_BASE..ID_CAP_BASE + 20).contains(&id) => {
            let i = (id - ID_CAP_BASE) as usize;
            if let Some((name, _)) = shared.captures.lock().unwrap().get(i).cloned() {
                let _ = cmd_tx.send(Cmd::SelectCapture(name));
            }
        }
        id if (ID_LAT_BASE..ID_LAT_BASE + LATENCY_PRESETS.len() as u16).contains(&id) => {
            let i = (id - ID_LAT_BASE) as usize;
            if let Some(frames) = LATENCY_PRESETS.get(i).copied() {
                let _ = cmd_tx.send(Cmd::SetLatency(frames));
            }
        }
        _ => {}
    }
}

fn encode(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
