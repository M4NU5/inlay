//! The settings window: server address and token, device name, microphone,
//! hold key and launch at login. Plain Win32 controls.

use crate::app::{self, Server};
use crate::icons::Look;
use crate::win::{from_wide, wide};
use crate::{audio, config, credentials, hotkey, startup, tray};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, Once};
use std::thread;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static FONT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
/// Microphone names behind the list entries after "System default".
static MICROPHONES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static TEST_RESULT: Mutex<Option<String>> = Mutex::new(None);

const ENDPOINT: i32 = 101;
const TOKEN: i32 = 102;
const NAME: i32 = 103;
const MICROPHONE: i32 = 104;
const HOTKEY: i32 = 105;
const LAUNCH: i32 = 106;
const STATUS: i32 = 107;
const TEST: i32 = 108;
const WM_TEST_DONE: u32 = WM_APP + 20;

/// The open settings window, or null. The message loop routes dialog keys to it.
pub fn window() -> HWND {
    WINDOW.load(Ordering::SeqCst)
}

pub fn close() {
    let window = window();
    if !window.is_null() {
        unsafe { DestroyWindow(window) };
    }
}

pub fn open() {
    let existing = window();
    if !existing.is_null() {
        unsafe {
            ShowWindow(existing, SW_SHOWNORMAL);
            SetForegroundWindow(existing);
        }
        return;
    }
    let class_name = wide("InlaySettings");
    unsafe {
        let instance = GetModuleHandleW(null());
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name.as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
                ..std::mem::zeroed()
            };
            RegisterClassW(&class);
        });
        let scale = GetDpiForSystem() as f32 / 96.0;
        let px = |value: i32| (value as f32 * scale).round() as i32;
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: px(460),
            bottom: px(314),
        };
        AdjustWindowRectEx(&mut frame, style, 0, 0);
        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;
        let title = wide("Inlay settings");
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            width,
            height,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return;
        }
        WINDOW.store(window, Ordering::SeqCst);
        let icon = tray::icon(Look::Idle) as LPARAM;
        SendMessageW(window, WM_SETICON, ICON_SMALL as WPARAM, icon);
        SendMessageW(window, WM_SETICON, ICON_BIG as WPARAM, icon);
        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&raw mut metrics).cast(),
            0,
        );
        FONT.store(
            CreateFontIndirectW(&metrics.lfMessageFont),
            Ordering::SeqCst,
        );

        let label = |text: &str, y: i32| {
            control(
                window,
                "STATIC",
                text,
                0,
                0,
                0,
                (px(16), px(y + 4), px(118), px(20)),
            )
        };
        let edit = |id: i32, extra: u32, y: i32| {
            control(
                window,
                "EDIT",
                "",
                WS_TABSTOP | ES_AUTOHSCROLL as u32 | extra,
                WS_EX_CLIENTEDGE,
                id,
                (px(140), px(y), px(304), px(24)),
            )
        };
        let list = |id: i32, y: i32| {
            control(
                window,
                "COMBOBOX",
                "",
                WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
                0,
                id,
                (px(140), px(y), px(304), px(240)),
            )
        };
        label("Server address", 16);
        edit(ENDPOINT, 0, 16);
        label("Server token", 50);
        edit(TOKEN, ES_PASSWORD as u32, 50);
        label("Device name", 84);
        edit(NAME, 0, 84);
        label("Microphone", 118);
        list(MICROPHONE, 118);
        label("Hold to talk", 152);
        list(HOTKEY, 152);
        control(
            window,
            "BUTTON",
            "Start Inlay when I sign in to Windows",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            0,
            LAUNCH,
            (px(140), px(188), px(304), px(22)),
        );
        control(
            window,
            "STATIC",
            "",
            0,
            0,
            STATUS,
            (px(16), px(220), px(428), px(40)),
        );
        control(
            window,
            "BUTTON",
            "Test connection",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            TEST,
            (px(16), px(268), px(130), px(30)),
        );
        control(
            window,
            "BUTTON",
            "Save",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            0,
            IDOK,
            (px(272), px(268), px(80), px(30)),
        );
        control(
            window,
            "BUTTON",
            "Cancel",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            IDCANCEL,
            (px(364), px(268), px(80), px(30)),
        );

        fill(window);
        ShowWindow(window, SW_SHOWNORMAL);
        SetForegroundWindow(window);
        SetFocus(GetDlgItem(window, ENDPOINT));
    }
}

unsafe fn control(
    parent: HWND,
    class: &str,
    text: &str,
    style: u32,
    extended: u32,
    id: i32,
    (x, y, width, height): (i32, i32, i32, i32),
) -> HWND {
    let (class, text) = (wide(class), wide(text));
    unsafe {
        let control = CreateWindowExW(
            extended,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            width,
            height,
            parent,
            id as usize as HMENU,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(
            control,
            WM_SETFONT,
            FONT.load(Ordering::SeqCst) as WPARAM,
            1,
        );
        control
    }
}

fn fill(window: HWND) {
    let app = app::shared();
    let settings = app.settings();
    set_text(window, ENDPOINT, &settings.config.endpoint);
    set_text(window, TOKEN, &settings.token);
    set_text(window, NAME, &settings.config.device_name);

    let connected = audio::input_names();
    let saved = settings.config.microphone.clone();
    let mut names = connected.clone();
    if let Some(saved) = &saved
        && !names.contains(saved)
    {
        names.push(saved.clone());
    }
    add_item(window, MICROPHONE, "System default");
    let mut selected = 0;
    for (index, name) in names.iter().enumerate() {
        add_item(
            window,
            MICROPHONE,
            &if connected.contains(name) {
                name.clone()
            } else {
                format!("{name} (not connected)")
            },
        );
        if saved.as_ref() == Some(name) {
            selected = index + 1;
        }
    }
    select(window, MICROPHONE, selected);
    *MICROPHONES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = names;

    for key in hotkey::KEYS {
        add_item(window, HOTKEY, key.label);
    }
    let current = hotkey::KEYS
        .iter()
        .position(|key| key.id == settings.config.hotkey)
        .unwrap_or(0);
    select(window, HOTKEY, current);

    let check = if startup::is_enabled() {
        BST_CHECKED
    } else {
        BST_UNCHECKED
    };
    unsafe { SendMessageW(GetDlgItem(window, LAUNCH), BM_SETCHECK, check as WPARAM, 0) };

    let status = match app.status(|status| status.server.clone()) {
        Server::Ready => "The server is ready.".to_string(),
        Server::Checking => "Checking the server…".to_string(),
        Server::Unavailable(message) => message,
    };
    set_text(window, STATUS, &status);
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            match (wparam & 0xFFFF) as i32 {
                IDOK => match save(window) {
                    Ok(()) => unsafe {
                        DestroyWindow(window);
                    },
                    Err(message) => set_text(window, STATUS, &message),
                },
                IDCANCEL => unsafe {
                    DestroyWindow(window);
                },
                TEST => test(window),
                _ => {}
            }
            0
        }
        WM_TEST_DONE => {
            if let Some(result) = TEST_RESULT.lock().ok().and_then(|mut result| result.take()) {
                set_text(window, STATUS, &result);
            }
            0
        }
        WM_DESTROY => {
            WINDOW.store(null_mut(), Ordering::SeqCst);
            let font = FONT.swap(null_mut(), Ordering::SeqCst);
            if !font.is_null() {
                unsafe { DeleteObject(font) };
            }
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn save(window: HWND) -> Result<(), String> {
    let endpoint = config::normalize_endpoint(&text(window, ENDPOINT))?;
    let token = config::validate_token(&text(window, TOKEN))?;
    let name = config::validate_device_name(&text(window, NAME))?;
    let microphone = match selection(window, MICROPHONE) {
        0 => None,
        index => MICROPHONES
            .lock()
            .ok()
            .and_then(|names| names.get(index - 1).cloned()),
    };
    let hotkey = hotkey::KEYS
        .get(selection(window, HOTKEY))
        .unwrap_or(&hotkey::KEYS[0])
        .id;
    let launch = unsafe { SendMessageW(GetDlgItem(window, LAUNCH), BM_GETCHECK, 0, 0) }
        == BST_CHECKED as isize;

    credentials::write(&endpoint, &token)?;
    let app = app::shared();
    let mut settings = app.settings();
    settings.config.endpoint = endpoint;
    settings.config.device_name = name;
    settings.config.microphone = microphone;
    settings.config.hotkey = hotkey.to_string();
    settings.token = token;
    settings
        .config
        .save()
        .map_err(|error| format!("Could not save settings: {error}"))?;
    if launch != startup::is_enabled() {
        startup::set(launch)?;
    }
    log!("settings: saved");
    app.apply_settings(settings);
    Ok(())
}

/// Checks the address and token as typed, without saving them.
fn test(window: HWND) {
    let endpoint = match config::normalize_endpoint(&text(window, ENDPOINT)) {
        Ok(endpoint) => endpoint,
        Err(message) => return set_text(window, STATUS, &message),
    };
    let token = match config::validate_token(&text(window, TOKEN)) {
        Ok(token) => token,
        Err(message) => return set_text(window, STATUS, &message),
    };
    set_text(window, STATUS, "Checking…");
    let target = window as usize;
    thread::spawn(move || {
        let result = match app::probe(&endpoint, &token) {
            Ok(message) => format!("Connected, and the token works. {message}"),
            Err(message) => message,
        };
        if let Ok(mut slot) = TEST_RESULT.lock() {
            *slot = Some(result);
        }
        unsafe { PostMessageW(target as HWND, WM_TEST_DONE, 0, 0) };
    });
}

fn text(window: HWND, id: i32) -> String {
    unsafe {
        let control = GetDlgItem(window, id);
        let length = GetWindowTextLengthW(control).max(0) as usize;
        let mut buffer = vec![0u16; length + 1];
        GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32);
        from_wide(&buffer)
    }
}

fn set_text(window: HWND, id: i32, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(GetDlgItem(window, id), value.as_ptr()) };
}

fn add_item(window: HWND, id: i32, value: &str) {
    let value = wide(value);
    unsafe {
        SendMessageW(
            GetDlgItem(window, id),
            CB_ADDSTRING,
            0,
            value.as_ptr() as LPARAM,
        )
    };
}

fn select(window: HWND, id: i32, index: usize) {
    unsafe { SendMessageW(GetDlgItem(window, id), CB_SETCURSEL, index, 0) };
}

fn selection(window: HWND, id: i32) -> usize {
    unsafe { SendMessageW(GetDlgItem(window, id), CB_GETCURSEL, 0, 0) }.max(0) as usize
}
