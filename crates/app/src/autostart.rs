//! Per-user Startup-folder shortcut (`airplay.lnk`). Not a Run key, not a service.

#![cfg(windows)]

use anyhow::{Context, Result};
use std::path::PathBuf;
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED, IPersistFile,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, SHGetKnownFolderPath, FOLDERID_Startup, KF_FLAG_DEFAULT, ShellLink,
};

const LINK_NAME: &str = "airplay.lnk";

pub fn apply(enabled: bool) -> Result<()> {
    std::thread::Builder::new()
        .name("airplay-autostart".into())
        .spawn(move || apply_sta(enabled))
        .context("spawn autostart")?
        .join()
        .map_err(|_| anyhow::anyhow!("autostart thread panicked"))?
}

fn apply_sta(enabled: bool) -> Result<()> {
    let path = link_path()?;
    if !enabled {
        return remove_link(&path);
    }
    let exe = std::env::current_exe().context("current_exe for autostart")?;
    let work = exe
        .parent()
        .map(PathBuf::from)
        .context("exe has no parent directory")?;

    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let result = write_link(&path, &exe, &work);
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    result
}

fn write_link(path: &std::path::Path, exe: &std::path::Path, work: &std::path::Path) -> Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .context("CoCreateInstance ShellLink")?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))
            .context("IShellLinkW::SetPath")?;
        link.SetWorkingDirectory(&HSTRING::from(work.as_os_str()))
            .context("IShellLinkW::SetWorkingDirectory")?;
        let persist: IPersistFile = link.cast().context("IShellLinkW -> IPersistFile")?;
        persist
            .Save(&HSTRING::from(path.as_os_str()), true)
            .context("IPersistFile::Save airplay.lnk")?;
    }
    Ok(())
}

fn remove_link(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("remove Startup airplay.lnk"),
    }
}

fn link_path() -> Result<PathBuf> {
    unsafe {
        let pwstr = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None)
            .context("SHGetKnownFolderPath FOLDERID_Startup")?;
        let dir = pwstr.to_string().context("Startup folder path")?;
        CoTaskMemFree(Some(pwstr.0 as *const std::ffi::c_void as *mut _));
        Ok(PathBuf::from(dir).join(LINK_NAME))
    }
}
