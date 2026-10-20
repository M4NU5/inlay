//! Mutes the default speakers while recording, as the Mac client's
//! `SystemOutputMuter` does. Uses the same mute as the volume flyout and media
//! keys; volume levels are never changed. Only a mute this app made is undone,
//! so speakers the user had already muted stay muted.

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::core::{GUID, HSTRING};

struct State {
    /// Takes holding a `Muted`. Overlapping takes share one mute, undone
    /// when the last of them stops recording.
    owners: usize,
    /// Endpoint IDs of speakers this app muted and hasn't unmuted yet,
    /// including any a crashed run left behind. An unmute that fails, say
    /// because the speakers are unplugged, stays here to be tried again.
    muted: Vec<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    owners: 0,
    muted: Vec::new(),
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

/// Mirrors `State::muted` on disk, so a crash between mute and restore can't
/// strand a silent PC: the next start unmutes them.
fn record() -> PathBuf {
    crate::config::data_dir().join("output-mute.json")
}

/// Keeps the speakers muted until dropped.
pub struct Muted(());

pub fn mute() -> Muted {
    let mut state = state();
    state.owners += 1;
    if state.owners > 1 {
        return Muted(());
    }
    let result = with_com(|| unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
        if volume.GetMute()?.as_bool() {
            return Ok(());
        }
        // Recorded first: if the process dies right after muting, the next
        // start still knows what to restore. Without the record, don't mute.
        state.muted.push(device.GetId()?.to_string()?);
        if !save(&state.muted) {
            state.muted.pop();
            return Ok(());
        }
        if let Err(error) = volume.SetMute(true, &GUID::zeroed()) {
            state.muted.pop();
            save(&state.muted);
            return Err(error);
        }
        log!("mute: muted the speakers while recording");
        Ok(())
    });
    if let Err(error) = result {
        log!("mute: could not mute the speakers: {error}");
    }
    Muted(())
}

impl Drop for Muted {
    fn drop(&mut self) {
        let mut state = state();
        state.owners -= 1;
        if state.owners == 0 {
            restore(&mut state);
        }
    }
}

/// Unmutes speakers a previous run muted and never restored.
pub fn restore_persisted() {
    let Ok(bytes) = fs::read(record()) else {
        return;
    };
    let mut state = state();
    state.muted = serde_json::from_slice(&bytes).unwrap_or_default();
    restore(&mut state);
}

fn restore(state: &mut State) {
    let before = state.muted.len();
    state.muted.retain(|id| match unmute(id) {
        Ok(()) => false,
        Err(error) => {
            log!("mute: could not unmute the speakers: {error}");
            true
        }
    });
    if state.muted.len() < before {
        log!("mute: unmuted the speakers");
    }
    save(&state.muted);
}

fn unmute(id: &str) -> windows::core::Result<()> {
    with_com(|| unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let volume: IAudioEndpointVolume = devices
            .GetDevice(&HSTRING::from(id))?
            .Activate(CLSCTX_ALL, None)?;
        if volume.GetMute()?.as_bool() {
            volume.SetMute(false, &GUID::zeroed())?;
        }
        Ok(())
    })
}

/// Writes the record, or removes it when nothing is muted, and says whether
/// that worked.
fn save(muted: &[String]) -> bool {
    let path = record();
    let result = if !muted.is_empty() {
        serde_json::to_vec(muted)
            .map_err(std::io::Error::other)
            .and_then(|bytes| fs::write(&path, bytes))
    } else if path.exists() {
        fs::remove_file(&path)
    } else {
        Ok(())
    };
    if let Err(error) = &result {
        log!("mute: could not update {}: {error}", path.display());
    }
    result.is_ok()
}

/// Runs `work` with COM available on this thread. cpal may already have set
/// up the thread as single-threaded, which serves just as well.
fn with_com<T>(work: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T> {
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let result = work();
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}
