//! Dictation key detection with a low-level keyboard hook (`WH_KEYBOARD_LL`).
//! The hook only observes. It never swallows or rewrites a key, so the chosen
//! key keeps working in other apps. It forwards edges to the controller, which
//! applies the trigger, the acceptance delay and the chord rule.

use crate::config::Config;
use crate::controller::Event;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

pub const DEFAULT_KEY: &str = "rightCtrl";

pub struct Key {
    pub id: &'static str,
    pub label: &'static str,
    pub vk: VIRTUAL_KEY,
}

/// Right Ctrl is the default because Right Alt is AltGr on UK and many other layouts.
pub const KEYS: &[Key] = &[
    Key {
        id: "rightCtrl",
        label: "Right Ctrl",
        vk: VK_RCONTROL,
    },
    Key {
        id: "rightAlt",
        label: "Right Alt",
        vk: VK_RMENU,
    },
    Key {
        id: "rightShift",
        label: "Right Shift",
        vk: VK_RSHIFT,
    },
    Key {
        id: "scrollLock",
        label: "Scroll Lock",
        vk: VK_SCROLL,
    },
    Key {
        id: "pause",
        label: "Pause",
        vk: VK_PAUSE,
    },
    Key {
        id: "f13",
        label: "F13",
        vk: VK_F13,
    },
    Key {
        id: "f14",
        label: "F14",
        vk: VK_F14,
    },
    Key {
        id: "f15",
        label: "F15",
        vk: VK_F15,
    },
    Key {
        id: "f16",
        label: "F16",
        vk: VK_F16,
    },
    Key {
        id: "f17",
        label: "F17",
        vk: VK_F17,
    },
    Key {
        id: "f18",
        label: "F18",
        vk: VK_F18,
    },
    Key {
        id: "f19",
        label: "F19",
        vk: VK_F19,
    },
    Key {
        id: "f20",
        label: "F20",
        vk: VK_F20,
    },
    Key {
        id: "f21",
        label: "F21",
        vk: VK_F21,
    },
    Key {
        id: "f22",
        label: "F22",
        vk: VK_F22,
    },
    Key {
        id: "f23",
        label: "F23",
        vk: VK_F23,
    },
    Key {
        id: "f24",
        label: "F24",
        vk: VK_F24,
    },
];

pub fn key(id: &str) -> &'static Key {
    KEYS.iter().find(|key| key.id == id).unwrap_or(&KEYS[0])
}

/// How the dictation key starts and stops a take, as on the Mac.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Hold,
    /// Two taps start recording, and two more stop it.
    DoubleTap,
    /// One tap starts recording, and the next stops it.
    Tap,
}

pub const TRIGGERS: [Trigger; 3] = [Trigger::Hold, Trigger::DoubleTap, Trigger::Tap];

impl Trigger {
    /// The Mac's token in the config file.
    pub fn id(self) -> &'static str {
        match self {
            Trigger::Hold => "hold",
            Trigger::DoubleTap => "doubleTap",
            Trigger::Tap => "tapToggle",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Trigger::Hold => "Hold to talk",
            Trigger::DoubleTap => "Double tap to toggle",
            Trigger::Tap => "Tap to start and stop",
        }
    }

    /// What to do with the key, as in "Hold Right Ctrl to talk".
    pub fn verb(self) -> &'static str {
        match self {
            Trigger::Hold => "Hold",
            Trigger::DoubleTap => "Double tap",
            Trigger::Tap => "Tap",
        }
    }

    pub fn from_id(id: &str) -> Trigger {
        TRIGGERS
            .into_iter()
            .find(|trigger| trigger.id() == id)
            .unwrap_or(Trigger::Hold)
    }
}

/// Tags keystrokes Inlay injects, so the hook ignores its own paste chord.
pub const INJECTED_MARKER: usize = 0x494E_4C41;

#[derive(Debug)]
pub enum Edge {
    Pressed,
    Released,
    /// Another key was newly pressed while the hold key was held: a shortcut, not dictation.
    OtherKey,
}

static VK: AtomicU16 = AtomicU16::new(VK_RCONTROL);
/// The index in `TRIGGERS` of the current trigger.
static TRIGGER: AtomicU8 = AtomicU8::new(0);
static HELD: AtomicBool = AtomicBool::new(false);
/// Which keys the hook has seen go down and not yet come up.
static DOWN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static SINK: OnceLock<Sender<Event>> = OnceLock::new();

/// Listens for the config's dictation key and trigger.
pub fn apply(config: &Config) {
    let vk = key(&config.hotkey).vk;
    if VK.swap(vk, Ordering::SeqCst) != vk {
        HELD.store(false, Ordering::SeqCst);
    }
    let trigger = Trigger::from_id(&config.activation_mode);
    let index = TRIGGERS.iter().position(|each| *each == trigger);
    TRIGGER.store(index.unwrap_or(0) as u8, Ordering::SeqCst);
}

pub fn current_trigger() -> Trigger {
    TRIGGERS[usize::from(TRIGGER.load(Ordering::SeqCst))]
}

pub fn current_key() -> VIRTUAL_KEY {
    VK.load(Ordering::SeqCst)
}

/// Whether the hook has seen the hold key go down and not yet come up.
pub fn is_held() -> bool {
    HELD.load(Ordering::SeqCst)
}

/// Recovers from a key-up the hook never saw (for example during a UAC prompt).
pub fn forget_hold() {
    HELD.store(false, Ordering::SeqCst);
}

/// The live physical state, independent of the hook.
pub fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(i32::from(vk)) as u16) & 0x8000 != 0 }
}

/// Installs the hook on its own thread, which must pump messages for it.
pub fn start(sink: Sender<Event>) -> Result<(), String> {
    let _ = SINK.set(sink);
    let (ready, installed) = mpsc::channel();
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), GetModuleHandleW(null()), 0);
            let _ = ready.send(!hook.is_null());
            if hook.is_null() {
                return;
            }
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            UnhookWindowsHookEx(hook);
        })
        .map_err(|error| error.to_string())?;
    match installed.recv() {
        Ok(true) => Ok(()),
        _ => Err("Windows refused the keyboard hook, so the hold-to-talk key cannot work.".into()),
    }
}

/// Must return quickly: Windows drops hooks that stall input.
unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if event.dwExtraInfo != INJECTED_MARKER {
            observe(wparam as u32, event);
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn observe(message: u32, event: &KBDLLHOOKSTRUCT) {
    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
    let vk = sided(event);
    let slot = &DOWN[usize::from(vk & 0xFF)];
    let fresh = down && !slot.swap(true, Ordering::SeqCst);
    if up {
        slot.store(false, Ordering::SeqCst);
    }
    let edge = if vk == VK.load(Ordering::SeqCst) {
        if down && !HELD.swap(true, Ordering::SeqCst) {
            refresh_down_keys();
            Some(Edge::Pressed)
        } else if up && HELD.swap(false, Ordering::SeqCst) {
            Some(Edge::Released)
        } else {
            None // Auto-repeat while held.
        }
    } else if fresh && HELD.load(Ordering::SeqCst) {
        Some(Edge::OtherKey)
    } else {
        None
    };
    if let (Some(edge), Some(sink)) = (edge, SINK.get()) {
        let _ = sink.send(Event::Hotkey(edge));
    }
}

/// Keys already down when a hold starts, such as a movement key in a game or
/// a stuck key, keep auto-repeating. Their repeats are not a new shortcut.
fn refresh_down_keys() {
    for (vk, slot) in DOWN.iter().enumerate().skip(1) {
        slot.store(is_down(vk as VIRTUAL_KEY), Ordering::SeqCst);
    }
}

/// Low-level hooks normally report left/right modifier codes already; this
/// also handles drivers that send the generic code with the extended flag.
fn sided(event: &KBDLLHOOKSTRUCT) -> VIRTUAL_KEY {
    let extended = event.flags & LLKHF_EXTENDED != 0;
    match event.vkCode as VIRTUAL_KEY {
        VK_CONTROL if extended => VK_RCONTROL,
        VK_CONTROL => VK_LCONTROL,
        VK_MENU if extended => VK_RMENU,
        VK_MENU => VK_LMENU,
        VK_SHIFT if event.scanCode == 0x36 => VK_RSHIFT,
        VK_SHIFT => VK_LSHIFT,
        vk => vk,
    }
}
