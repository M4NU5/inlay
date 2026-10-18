//! Plain paste: stage the transcript on the clipboard, send Ctrl+V (Ctrl+Shift+V
//! in consoles), then put the previous clipboard back. Windows gives no signal
//! that the text landed, so nothing here claims it did. Windows also blocks
//! input into elevated (administrator) windows from this unelevated app.

use crate::hotkey::{self, INJECTED_MARKER};
use crate::win::{from_wide, wide};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{
    CF_BITMAP, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_ENHMETAFILE,
    CF_METAFILEPICT, CF_OWNERDISPLAY, CF_PALETTE, CF_UNICODETEXT,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_LCONTROL,
    VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetClassNameW, GetForegroundWindow, HWND_MESSAGE,
};

/// Console hosts paste with Ctrl+Shift+V; Ctrl+V can reach the shell as ^V.
const TERMINAL_CLASSES: [&str; 2] = ["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"];
/// Long enough for apps to read the clipboard after Ctrl+V, short enough that
/// the user's own clipboard comes back before they need it.
const RESTORE_DELAY: Duration = Duration::from_millis(500);
const MAX_FORMAT_BYTES: usize = 64 * 1024 * 1024;
const VK_V: VIRTUAL_KEY = 0x56;
/// An unassigned key code. Pressing it before releasing Alt or Win stops that
/// release from opening the menu bar or the Start menu.
const VK_MASK: VIRTUAL_KEY = 0xE8;

pub fn paste(text: &str) -> Result<(), String> {
    let saved = snapshot()?;
    write_text(text, true)?;
    let staged = unsafe { GetClipboardSequenceNumber() };
    let target = unsafe { GetForegroundWindow() };
    if target.is_null() {
        restore(&saved, staged);
        return Err("No window has keyboard focus.".into());
    }
    let terminal = is_terminal(target);
    release_modifiers();
    if !send_paste(terminal) {
        restore(&saved, staged);
        return Err("Windows blocked the paste keystroke.".into());
    }
    log!(
        "paste: sent {} to the foreground window",
        if terminal { "Ctrl+Shift+V" } else { "Ctrl+V" }
    );
    thread::sleep(RESTORE_DELAY);
    restore(&saved, staged);
    Ok(())
}

/// Leaves `text` on the clipboard, where clipboard history may keep it.
pub fn copy(text: &str) -> Result<(), String> {
    write_text(text, false)
}

/// Opens the clipboard for one operation, owned by a short-lived message-only
/// window. The window is gone before anything waits, so other apps never block
/// on clipboard messages sent to it.
struct Clipboard {
    owner: HWND,
}

impl Clipboard {
    fn open() -> Result<Clipboard, String> {
        let class = wide("STATIC");
        let owner = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        if owner.is_null() {
            return Err("Windows could not prepare the clipboard.".into());
        }
        for _ in 0..20 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Clipboard { owner });
            }
            thread::sleep(Duration::from_millis(25));
        }
        unsafe { DestroyWindow(owner) };
        Err("Another app is holding the clipboard.".into())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
            DestroyWindow(self.owner);
        }
    }
}

/// Copies every format that is plain global memory: text, HTML, RTF, file
/// lists and images (as CF_DIB/CF_DIBV5).
fn snapshot() -> Result<Vec<(u32, Vec<u8>)>, String> {
    let _clipboard = Clipboard::open()?;
    let mut formats = Vec::new();
    let mut format = 0;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if !copyable(format) {
            continue;
        }
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                continue;
            }
            let size = GlobalSize(handle);
            if size == 0 || size > MAX_FORMAT_BYTES {
                continue;
            }
            let pointer = GlobalLock(handle) as *const u8;
            if pointer.is_null() {
                continue;
            }
            formats.push((format, std::slice::from_raw_parts(pointer, size).to_vec()));
            GlobalUnlock(handle);
        }
    }
    Ok(formats)
}

fn copyable(format: u32) -> bool {
    // GDI and metafile handles are not global memory. CF_DIB covers bitmaps.
    let handles = [
        CF_BITMAP,
        CF_DSPBITMAP,
        CF_ENHMETAFILE,
        CF_DSPENHMETAFILE,
        CF_METAFILEPICT,
        CF_DSPMETAFILEPICT,
        CF_PALETTE,
        CF_OWNERDISPLAY,
    ];
    if handles.iter().any(|&handle| u32::from(handle) == format) {
        return false;
    }
    if format < 0xC000 {
        return true;
    }
    // OLE's own formats describe the original data object, which is gone once
    // Inlay takes the clipboard. Restoring them would point at nothing.
    let mut name = [0u16; 64];
    let length = unsafe { GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32) };
    let name = from_wide(&name[..length.max(0) as usize]);
    !matches!(name.as_str(), "DataObject" | "Ole Private Data")
}

/// `private` adds the markers that keep the staged transcript out of clipboard
/// history, cloud clipboard and clipboard managers.
fn write_text(text: &str, private: bool) -> Result<(), String> {
    let units: Vec<u16> = text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let _clipboard = Clipboard::open()?;
    if unsafe { EmptyClipboard() } == 0 {
        return Err("Windows would not let Inlay use the clipboard.".into());
    }
    set(u32::from(CF_UNICODETEXT), &bytes)?;
    if private {
        let markers = [
            ("ExcludeClipboardContentFromMonitorProcessing", 1u32),
            ("CanIncludeInClipboardHistory", 0),
            ("CanUploadToCloudClipboard", 0),
        ];
        for (name, value) in markers {
            let format = unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) };
            if format != 0 {
                let _ = set(format, &value.to_le_bytes());
            }
        }
    }
    Ok(())
}

/// Must run while the clipboard is open. The clipboard owns the memory on success.
fn set(format: u32, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if memory.is_null() {
            return Err("Out of memory for the clipboard.".into());
        }
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return Err("Out of memory for the clipboard.".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        GlobalUnlock(memory);
        if SetClipboardData(format, memory).is_null() {
            GlobalFree(memory);
            return Err("Windows rejected the clipboard data.".into());
        }
    }
    Ok(())
}

/// Puts the saved contents back, unless something newer was copied meanwhile.
fn restore(saved: &[(u32, Vec<u8>)], staged: u32) {
    if unsafe { GetClipboardSequenceNumber() } != staged {
        log!("paste: clipboard changed after staging; leaving the newer contents");
        return;
    }
    let Ok(_clipboard) = Clipboard::open() else {
        log!("paste: could not reopen the clipboard to restore it");
        return;
    };
    unsafe { EmptyClipboard() };
    for (format, bytes) in saved {
        let _ = set(*format, bytes);
    }
}

fn is_terminal(window: HWND) -> bool {
    let mut name = [0u16; 64];
    let length = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    let name = from_wide(&name[..length.max(0) as usize]);
    TERMINAL_CLASSES.contains(&name.as_str())
}

/// A modifier the user is still holding would turn Ctrl+V into another shortcut.
fn release_modifiers() {
    let modifiers = [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LMENU,
        VK_RMENU,
        VK_LWIN,
        VK_RWIN,
    ];
    let held: Vec<VIRTUAL_KEY> = modifiers
        .into_iter()
        .filter(|&vk| hotkey::is_down(vk))
        .collect();
    if held.is_empty() {
        return;
    }
    let mut inputs = Vec::new();
    if held
        .iter()
        .any(|vk| matches!(*vk, VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN))
    {
        inputs.push(key(VK_MASK, false));
        inputs.push(key(VK_MASK, true));
    }
    inputs.extend(held.iter().map(|&vk| key(vk, true)));
    send(&inputs);
}

fn send_paste(terminal: bool) -> bool {
    let mut press = vec![key(VK_LCONTROL, false)];
    let mut release = vec![key(VK_V, true)];
    if terminal {
        press.push(key(VK_LSHIFT, false));
        release.push(key(VK_LSHIFT, true));
    }
    press.push(key(VK_V, false));
    release.push(key(VK_LCONTROL, true));
    let pressed = send(&press);
    // Some apps miss a chord whose keys arrive in the same instant.
    thread::sleep(Duration::from_millis(20));
    let released = send(&release);
    pressed && released
}

fn send(inputs: &[INPUT]) -> bool {
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    sent as usize == inputs.len()
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    let mut flags: KEYBD_EVENT_FLAGS = if up { KEYEVENTF_KEYUP } else { 0 };
    if scan & 0xFF00 == 0xE000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: (scan & 0xFF) as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            },
        },
    }
}
