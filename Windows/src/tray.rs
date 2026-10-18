//! The notification-area icon, its menu, and the hidden window that receives
//! tray clicks and refresh requests from other threads.

use crate::app::{self, App, Server};
use crate::controller::Event;
use crate::icons::{self, Look};
use crate::win::{fill, wide};
use crate::{hotkey, paste, settings};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// Icons for each look, created once on the UI thread and kept for the process.
static ICONS: OnceLock<[usize; 4]> = OnceLock::new();

const WM_TRAY: u32 = WM_APP + 1;
const WM_REFRESH: u32 = WM_APP + 2;
const WM_NOTIFY_ERROR: u32 = WM_APP + 3;
const NOTICE_TIMER: usize = 1;
const ID_STATUS: usize = 10;
const ID_COPY: usize = 11;
const ID_SETTINGS: usize = 12;
const ID_QUIT: usize = 13;

/// Asks the UI thread to redraw the icon and tooltip. Safe from any thread.
pub fn refresh() {
    post(WM_REFRESH);
}

/// Asks the UI thread to show the latest error as a notification.
pub fn notify_error() {
    post(WM_NOTIFY_ERROR);
}

fn post(message: u32) {
    let window = WINDOW.load(Ordering::SeqCst);
    if !window.is_null() {
        unsafe { PostMessageW(window, message, 0, 0) };
    }
}

/// Runs the UI thread's message loop. `first_run` opens settings straight away.
pub fn run(first_run: bool) -> Result<(), String> {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide("InlayTray");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::SeqCst,
        );
        let title = wide("Inlay");
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return Err("Windows could not create the tray window.".into());
        }
        let _ = ICONS.set(
            [Look::Idle, Look::Recording, Look::Processing, Look::Error]
                .map(|look| icons::create(look) as usize),
        );
        WINDOW.store(window, Ordering::SeqCst);
        add_icon(window);
        if first_run {
            settings::open();
        }
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            let dialog = settings::window();
            if !dialog.is_null() && IsDialogMessageW(dialog, &message) != 0 {
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAY => {
            if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                show_menu(window);
            }
            0
        }
        WM_REFRESH => {
            update_icon(window);
            0
        }
        WM_TIMER => {
            unsafe { KillTimer(window, NOTICE_TIMER) };
            update_icon(window);
            0
        }
        WM_NOTIFY_ERROR => {
            show_error(window);
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                ID_COPY => copy_last(),
                ID_SETTINGS => settings::open(),
                ID_QUIT => quit(window),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            let data = icon_data(window);
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
            }
            0
        }
        // Explorer restarted, so the icon must be added again.
        _ if message != 0 && message == TASKBAR_CREATED.load(Ordering::SeqCst) => {
            add_icon(window);
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// The icon for `look`, shared with the settings window's title bar.
pub fn icon(look: Look) -> HICON {
    ICONS
        .get()
        .map_or(null_mut(), |icons| icons[look as usize] as HICON)
}

fn icon_data(window: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = 1;
    data
}

fn add_icon(window: HWND) {
    let mut data = icon_data(window);
    data.uFlags = NIF_MESSAGE;
    data.uCallbackMessage = WM_TRAY;
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
    update_icon(window);
}

/// The look and text for the current state, most urgent first.
fn describe(app: &App) -> (Look, String) {
    let key = hotkey::key(&app.settings().config.hotkey).label;
    app.status(|status| {
        let notice = status
            .notice
            .as_ref()
            .filter(|notice| notice.until > Instant::now());
        if status.recording {
            (Look::Recording, "Listening. Release to transcribe.".into())
        } else if status.processing > 0 {
            (Look::Processing, "Transcribing…".into())
        } else if let Some(notice) = notice {
            (
                if notice.error {
                    Look::Error
                } else {
                    Look::Idle
                },
                notice.text.clone(),
            )
        } else {
            match &status.server {
                Server::Ready => (Look::Idle, format!("Ready. Hold {key} to talk.")),
                Server::Checking => (Look::Idle, "Connecting to the server…".into()),
                Server::Unavailable(message) => (Look::Error, message.clone()),
            }
        }
    })
}

fn update_icon(window: HWND) {
    let app = app::shared();
    let (look, text) = describe(app);
    let mut data = icon_data(window);
    data.uFlags = NIF_ICON | NIF_TIP;
    data.hIcon = icon(look);
    fill(&mut data.szTip, &format!("Inlay: {text}"));
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    // Redraw once the current notice expires.
    let remaining = app.status(|status| {
        status
            .notice
            .as_ref()
            .map(|notice| notice.until.saturating_duration_since(Instant::now()))
    });
    if let Some(remaining) = remaining.filter(|remaining| !remaining.is_zero()) {
        unsafe {
            SetTimer(
                window,
                NOTICE_TIMER,
                (remaining + Duration::from_millis(50)).as_millis() as u32,
                None,
            )
        };
    }
}

fn show_error(window: HWND) {
    let text = app::shared().status(|status| {
        status
            .notice
            .as_ref()
            .filter(|notice| notice.error)
            .map(|notice| notice.text.clone())
    });
    let Some(text) = text else { return };
    let mut data = icon_data(window);
    data.uFlags = NIF_INFO;
    data.dwInfoFlags = NIIF_WARNING;
    fill(&mut data.szInfoTitle, "Inlay");
    fill(&mut data.szInfo, &text);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
}

fn show_menu(window: HWND) {
    let app = app::shared();
    let (_, status) = describe(app);
    let has_transcript = app.status(|status| status.last_transcript.is_some());
    unsafe {
        let menu = CreatePopupMenu();
        let items = [
            (MF_STRING | MF_GRAYED, ID_STATUS, status.as_str()),
            (MF_SEPARATOR, 0, ""),
            (
                MF_STRING | if has_transcript { 0 } else { MF_GRAYED },
                ID_COPY,
                "Copy last transcript",
            ),
            (MF_STRING, ID_SETTINGS, "Settings…"),
            (MF_SEPARATOR, 0, ""),
            (MF_STRING, ID_QUIT, "Quit Inlay"),
        ];
        for (flags, id, text) in items {
            let text = wide(text);
            AppendMenuW(menu, flags, id, text.as_ptr());
        }
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        // Required for the menu to close when the user clicks elsewhere.
        SetForegroundWindow(window);
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            0,
            window,
            null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }
}

fn copy_last() {
    let app = app::shared();
    let Some(text) = app.status(|status| status.last_transcript.clone()) else {
        return;
    };
    match paste::copy(&text) {
        Ok(()) => app.note("Copied the last transcript."),
        Err(message) => app.fail(&message),
    }
}

fn quit(window: HWND) {
    log!("quit requested");
    let (done, finished) = mpsc::channel();
    if app::shared().events.send(Event::Quit(done)).is_ok() {
        let _ = finished.recv_timeout(Duration::from_secs(3));
    }
    settings::close();
    unsafe { DestroyWindow(window) };
}
