//! Windows' microphone privacy switches, read from where Settings keeps them.
//! When one is off, recording gets only silence. A desktop app can't ask for
//! access as a Mac app can; it can only send the user to Settings.

use crate::win::{from_wide, wide};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const STORE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// The first switch in Settings that keeps Inlay from the microphone, in the
/// order Settings shows them. `None` when Inlay may record. "Let apps access
/// your microphone" governs Store apps only, so it doesn't count for Inlay.
pub fn microphone_blocked() -> Option<&'static str> {
    let desktop = format!(r"{STORE}\NonPackaged");
    [
        (HKEY_LOCAL_MACHINE, STORE, "Microphone access"),
        (
            HKEY_CURRENT_USER,
            desktop.as_str(),
            "Let desktop apps access your microphone",
        ),
    ]
    .into_iter()
    .find(|(root, key, _)| denied(*root, key))
    .map(|(_, _, switch)| switch)
}

/// A missing value is the Windows default, which allows access.
fn denied(root: HKEY, key: &str) -> bool {
    let (key, name) = (wide(key), wide("Value"));
    let mut value = [0u16; 16];
    let mut size = size_of_val(&value) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            value.as_mut_ptr().cast(),
            &mut size,
        )
    };
    status == ERROR_SUCCESS && from_wide(&value) == "Deny"
}

/// Opens Settings at Privacy & security > Microphone.
pub fn open_settings() -> Result<(), String> {
    let (verb, page) = (wide("open"), wide("ms-settings:privacy-microphone"));
    let opened = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            page.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success.
    if opened as usize > 32 {
        Ok(())
    } else {
        Err("Could not open Microphone settings.".into())
    }
}
