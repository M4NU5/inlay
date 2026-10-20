//! The hold-to-talk state machine. An accepted hold starts a take and its
//! release finishes it. Takes run on their own threads, so a new hold can start
//! while earlier takes are still uploading or transcribing. The window's Test
//! microphone button starts and finishes a test take the same way.

use crate::app::App;
use crate::hotkey::{self, Edge};
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

enum Phase {
    Idle,
    Pending(Instant),
    /// The hold turned into a shortcut or its take ended; wait for release.
    Blocked,
    Recording(take::Handle),
    /// A microphone test, finished by a second `Test`. The hold key is ignored.
    Testing(take::Handle),
}

pub fn run(app: Arc<App>, events: Receiver<Event>) {
    let mut phase = Phase::Idle;
    let mut takes = 0u64;
    let mut missed_releases = 0;
    loop {
        let timeout = match &phase {
            Phase::Idle | Phase::Testing(_) => None,
            Phase::Pending(since) => {
                Some(ACCEPT_DELAY.saturating_sub(since.elapsed()).min(WATCHDOG))
            }
            Phase::Blocked | Phase::Recording(_) => Some(WATCHDOG),
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
                if let Phase::Recording(take) | Phase::Testing(take) = phase {
                    take.cancel_and_wait("Inlay quit while recording.");
                }
                let _ = done.send(());
                return;
            }
            Some(event) => Some(event),
            // Watchdog tick: the hook may have missed the key-up.
            None if !matches!(phase, Phase::Idle | Phase::Testing(_))
                && !hotkey::is_down(hotkey::current_key()) =>
            {
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
        phase = match (phase, event) {
            (Phase::Idle, Some(Event::Hotkey(Edge::Pressed))) => Phase::Pending(Instant::now()),
            (Phase::Pending(_) | Phase::Blocked, Some(Event::Hotkey(Edge::Released))) => {
                Phase::Idle
            }
            (Phase::Pending(_), Some(Event::Hotkey(Edge::OtherKey))) => Phase::Blocked,
            (Phase::Recording(take), Some(Event::Hotkey(Edge::Released))) => {
                take.release();
                Phase::Idle
            }
            (Phase::Recording(take), Some(Event::Hotkey(Edge::OtherKey))) => {
                take.cancel("Cancelled: another key was pressed while holding the dictation key.");
                Phase::Blocked
            }
            (Phase::Recording(take), Some(Event::TakeEnded(id))) if take.id == id => {
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
