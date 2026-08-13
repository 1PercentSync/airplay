//! Windows tray: device pick, start/stop, HomePod volume, status tooltip.
//! Menu pattern follows Sunshine system_tray (Open/Quit style), without its web UI.
//! [evidence: Sunshine src/system_tray.h; windows-rs Shell_NotifyIconW]

#![cfg(windows)]

use crate::config::Config;
use crate::probe::{self, Discovered};
use crate::run::{self, SessionCtrl};
use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW, LoadIconW, PostQuitMessage,
    RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenu, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, IDI_APPLICATION, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_COMMAND, WM_DESTROY, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

const WM_TRAY: u32 = WM_APP + 1;
const ID_START: u16 = 1001;
const ID_STOP: u16 = 1002;
const ID_REFRESH: u16 = 1003;
const ID_QUIT: u16 = 1004;
const ID_VOL_BASE: u16 = 1100;
const ID_DEV_BASE: u16 = 1200;
const ID_CAP_BASE: u16 = 1300;

enum Cmd {
    Start,
    Stop,
    SetVolume(f64),
    SelectTarget { target: String, name: String },
    SelectCapture(String),
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
        running: AtomicBool::new(false),
        tooltip: Mutex::new("airplay: idle".into()),
    });
    refresh_captures(&shared);

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let cmd_quit = cmd_tx.clone();
    let rt = tokio::runtime::Runtime::new()?;
    let handle = rt.handle().clone();
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
    loop {
        let Some(cmd) = cmd_rx.recv().await else {
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
            }
            Cmd::SelectTarget { target, name } => {
                *shared.target.lock().unwrap() = target.clone();
                *shared.device_name.lock().unwrap() = name;
                save_cfg(&shared);
                info!(target = %target, "selected HomePod");
            }
            Cmd::SelectCapture(name) => {
                *shared.capture.lock().unwrap() = name.clone();
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
            Cmd::Start => {
                if slot.lock().unwrap().is_some() {
                    continue;
                }
                let target = shared.target.lock().unwrap().clone();
                if target.is_empty() {
                    warn!("no HomePod selected");
                    *shared.tooltip.lock().unwrap() = "airplay: no device".into();
                    continue;
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
                tokio::spawn(async move {
                    let status = {
                        let sh = sh.clone();
                        Arc::new(move |s: &str| {
                            info!("{s}");
                            let line = s.trim_start_matches("[STATUS] ").to_string();
                            *sh.tooltip.lock().unwrap() = format!("airplay: {line}");
                        }) as Arc<dyn Fn(&str) + Send + Sync>
                    };
                    status("[STATUS] connecting");
                    if let Err(e) =
                        run::run_supervised(target, hint, stop_rx, vol_rx, rec, status).await
                    {
                        error!("supervised session: {e}");
                    }
                    *slot_t.lock().unwrap() = None;
                    sh.running.store(false, Ordering::SeqCst);
                    *sh.tooltip.lock().unwrap() = "airplay: idle".into();
                });
            }
            Cmd::Stop => {
                if let Some(s) = slot.lock().unwrap().take() {
                    s.request_stop();
                }
                shared.running.store(false, Ordering::SeqCst);
                *shared.tooltip.lock().unwrap() = "airplay: idle".into();
            }
            Cmd::Quit => {
                if let Some(s) = slot.lock().unwrap().take() {
                    s.request_stop();
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
                break;
            }
        }
    }
}

fn save_cfg(shared: &Shared) {
    let cfg = Config {
        device_ip: shared.target.lock().unwrap().clone(),
        device_name: shared.device_name.lock().unwrap().clone(),
        capture_device: shared.capture.lock().unwrap().clone(),
        volume: *shared.volume.lock().unwrap(),
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

    let icon = LoadIconW(None, IDI_APPLICATION)?;
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

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let tip = shared.tooltip.lock().unwrap().clone();
        write_tip(&mut nid, &tip);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

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

    if msg == WM_TRAY && lparam.0 as u32 == WM_RBUTTONUP {
        show_menu(hwnd, shared, cmd_tx);
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
        _ => {}
    }
}

fn encode(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
