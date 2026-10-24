//! The dictation key's state machine. With Hold, an accepted hold starts a take
//! and its release finishes it. With Double tap or Tap, as on the Mac, taps
//! count as the key comes up: the take starts on one and keeps recording after
//! release until the next. Takes run on their own threads, so a new one can
//! start while earlier takes are still uploading or transcribing. The window's
//! Test microphone button starts and finishes a test take the same way.

use crate::app::App;
use crate::hotkey::{self, Edge, Trigger};
use crate::take;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Event {
    Hotkey(Edge),
    /// A take stopped recording on its own: time limit, error or cancellation.
    TakeEnded(u64),
    /// Test microphone: start a test take, or finish the one recording.
    Test,
    /// Quit: cancel any recording, then acknowledge.
    Quit(Sender<()>),
}

/// A press only starts recording after this long, so quick shortcuts such as
/// Ctrl+C with the hold key never open the microphone. Matches the Mac client.
const ACCEPT_DELAY: Duration = Duration::from_millis(180);
/// How often a held key's physical state is checked, in case its key-up was
/// never seen (for example when a UAC prompt took the keyboard).
const WATCHDOG: Duration = Duration::from_millis(100);
/// Two taps this close together make a double tap. Matches the Mac client.
const DOUBLE_TAP: Duration = Duration::from_millis(450);

enum Phase {
    Idle,
    Pending(Instant),
    /// The hold turned into a shortcut or its take ended; wait for release.
    Blocked,
    Recording(take::Handle),
    /// Double tap or Tap: the key is down, and its release may be a tap.
    Tapping,
    /// Double tap or Tap: recording until a tap stops it.
    Latched(take::Handle, Key),
    /// A microphone test, finished by a second `Test`. The hold key is ignored.
    Testing(take::Handle),
}

/// The key during a latched take.
enum Key {
    Up,
    Down,
    /// Down, and part of a shortcut, so its release isn't a tap.
    Shortcut,
}

impl Phase {
    /// Whether the key is down as far as this phase knows.
    fn holds_key(&self) -> bool {
        !matches!(
            self,
            Phase::Idle | Phase::Testing(_) | Phase::Latched(_, Key::Up)
        )
    }
}

pub fn run(app: Arc<App>, events: Receiver<Event>) {
    let mut phase = Phase::Idle;
    let mut takes = 0u64;
    let mut missed_releases = 0;
    // The last tap that didn't toggle, for a double tap.
    let mut last_tap: Option<Instant> = None;
    loop {
        let timeout = match &phase {
            Phase::Pending(since) => {
                Some(ACCEPT_DELAY.saturating_sub(since.elapsed()).min(WATCHDOG))
            }
            phase if phase.holds_key() => Some(WATCHDOG),
            _ => None,
        };
        let event = match timeout {
            None => match events.recv() {
                Ok(event) => Some(event),
                Err(_) => return,
            },
            Some(timeout) => match events.recv_timeout(timeout) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        let event = match event {
            Some(Event::Quit(done)) => {
                if let Phase::Recording(take) | Phase::Latched(take, _) | Phase::Testing(take) =
                    phase
                {
                    take.cancel_and_wait("Inlay quit while recording.");
                }
                let _ = done.send(());
                return;
            }
            Some(event) => Some(event),
            // Watchdog tick: the hook may have missed the key-up.
            None if phase.holds_key() && !hotkey::is_down(hotkey::current_key()) => {
                missed_releases += 1;
                if missed_releases < 2 {
                    None
                } else {
                    log!("hotkey: key-up not seen; treating the key as released");
                    hotkey::forget_hold();
                    Some(Event::Hotkey(Edge::Released))
                }
            }
            None => {
                missed_releases = 0;
                None
            }
        };
        if event.is_some() {
            missed_releases = 0;
        }
        // A tap toggles recording, except that Double tap needs a second one.
        let mut toggles = |at: Instant| {
            if hotkey::current_trigger() != Trigger::DoubleTap {
                return true;
            }
            let double = last_tap.is_some_and(|tap| at.duration_since(tap) <= DOUBLE_TAP);
            last_tap = if double { None } else { Some(at) };
            double
        };
        phase = match (phase, event) {
            (Phase::Idle, Some(Event::Hotkey(Edge::Pressed))) => {
                if hotkey::current_trigger() == Trigger::Hold {
                    Phase::Pending(Instant::now())
                } else {
                    Phase::Tapping
                }
            }
            (Phase::Pending(_) | Phase::Blocked, Some(Event::Hotkey(Edge::Released))) => {
                Phase::Idle
            }
            (Phase::Pending(_) | Phase::Tapping, Some(Event::Hotkey(Edge::OtherKey))) => {
                Phase::Blocked
            }
            (Phase::Tapping, Some(Event::Hotkey(Edge::Released))) => {
                if toggles(Instant::now()) {
                    takes += 1;
                    log!("hotkey: tap accepted; starting take {takes}");
                    Phase::Latched(take::start(takes, app.clone(), false), Key::Up)
                } else {
                    Phase::Idle
                }
            }
            (Phase::Latched(take, Key::Up), Some(Event::Hotkey(Edge::Pressed))) => {
                Phase::Latched(take, Key::Down)
            }
            // A shortcut doesn't stop a latched take, as on the Mac.
            (Phase::Latched(take, Key::Down), Some(Event::Hotkey(Edge::OtherKey))) => {
                Phase::Latched(take, Key::Shortcut)
            }
            (Phase::Latched(take, Key::Shortcut), Some(Event::Hotkey(Edge::Released))) => {
                Phase::Latched(take, Key::Up)
            }
            (Phase::Latched(take, Key::Down), Some(Event::Hotkey(Edge::Released))) => {
                if toggles(Instant::now()) {
                    log!("hotkey: tap accepted; finishing take {}", take.id);
                    take.release();
                    Phase::Idle
                } else {
                    Phase::Latched(take, Key::Up)
                }
            }
            (Phase::Recording(take), Some(Event::Hotkey(Edge::Released))) => {
                take.release();
                Phase::Idle
            }
            (Phase::Recording(take), Some(Event::Hotkey(Edge::OtherKey))) => {
                take.cancel("Cancelled: another key was pressed while holding the dictation key.");
                Phase::Blocked
            }
            (Phase::Recording(take) | Phase::Latched(take, _), Some(Event::TakeEnded(id)))
                if take.id == id =>
            {
                last_tap = None;
                if hotkey::is_held() {
                    Phase::Blocked
                } else {
                    Phase::Idle
                }
            }
            (Phase::Pending(since), None) if since.elapsed() >= ACCEPT_DELAY => {
                takes += 1;
                log!("hotkey: hold accepted; starting take {takes}");
                Phase::Recording(take::start(takes, app.clone(), false))
            }
            (Phase::Idle, Some(Event::Test)) => {
                takes += 1;
                log!("test: starting take {takes}");
                Phase::Testing(take::start(takes, app.clone(), true))
            }
            (Phase::Testing(take), Some(Event::Test)) => {
                take.release();
                Phase::Idle
            }
            (Phase::Testing(take), Some(Event::TakeEnded(id))) if take.id == id => Phase::Idle,
            (phase, _) => phase,
        };
    }
}
