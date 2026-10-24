//! Inlay for Windows: a tray app that records while a key is held, uploads the
//! audio to an Inlay server, and pastes the transcript at the cursor.
#![windows_subsystem = "windows"]

#[macro_use]
mod log;
mod api;
mod app;
mod audio;
mod config;
mod controller;
mod credentials;
mod hotkey;
mod icons;
mod menu;
mod mute;
mod overlay;
mod paste;
mod privacy;
mod settings;
mod startup;
mod take;
mod theme;
mod tray;
mod win;

use app::{App, Settings};
use config::Config;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use std::{fs, thread};
use win::wide;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MessageBoxW,
};

fn main() -> ExitCode {
    let directory = config::data_dir();
    let _ = fs::create_dir_all(&directory);
    log::init(directory.join("inlay.log"));

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, text] = arguments.as_slice()
        && flag == "--paste"
    {
        return paste_command(text);
    }

    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE) };
    // One instance per data directory. Mutex names cannot contain backslashes.
    let name = wide(&format!(
        "Local\\Inlay|{}",
        directory.display().to_string().replace('\\', "/")
    ));
    let instance = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if !instance.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        message(
            "Inlay is already running. Look for its icon in the notification area.",
            MB_ICONINFORMATION,
        );
        return ExitCode::SUCCESS;
    }
    log!("Inlay for Windows {} starting", env!("CARGO_PKG_VERSION"));
    mute::restore_persisted();

    let first_run = !directory.join("config.json").exists();
    let config = Config::load();
    let token = credentials::read(&config.endpoint);
    hotkey::apply(&config);
    let (events, inbox) = mpsc::channel();
    let (wake, woken) = mpsc::channel();
    let app = Arc::new(App::new(Settings { config, token }, events.clone(), wake));
    app::install(app.clone());

    if let Err(error) = hotkey::start(events) {
        log!("startup: {error}");
        message(&error, MB_ICONWARNING);
        return ExitCode::FAILURE;
    }
    let controller = app.clone();
    thread::Builder::new()
        .name("controller".into())
        .spawn(move || controller::run(controller, inbox))
        .expect("controller thread");
    let monitor = app.clone();
    thread::Builder::new()
        .name("health".into())
        .spawn(move || app::monitor(monitor, woken))
        .expect("health thread");

    if let Err(error) = tray::run(first_run) {
        log!("startup: {error}");
        message(&error, MB_ICONWARNING);
        return ExitCode::FAILURE;
    }
    log!("Inlay for Windows stopped");
    ExitCode::SUCCESS
}

/// `inlay.exe --paste TEXT` waits two seconds, then pastes TEXT into the
/// focused window with the same clipboard save, Ctrl+V and restore as a take,
/// or copies it when nothing focused takes text.
/// It is a manual check for paste problems in a particular app.
fn paste_command(text: &str) -> ExitCode {
    thread::sleep(Duration::from_secs(2));
    match paste::paste(text, Config::load().preserve_clipboard, |_| {}) {
        Ok(paste::Delivered::Pasted) => {
            log!("--paste: sent");
            ExitCode::SUCCESS
        }
        Ok(paste::Delivered::Copied) => {
            log!("--paste: copied");
            ExitCode::SUCCESS
        }
        Err(error) => {
            log!("--paste: {error}");
            ExitCode::FAILURE
        }
    }
}

fn message(text: &str, icon: u32) {
    let (text, title) = (wide(text), wide("Inlay"));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | icon,
        )
    };
}
