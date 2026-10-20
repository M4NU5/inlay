//! State shared by the tray, the controller and the take threads.

use crate::api::{API_VERSION, Api};
use crate::config::Config;
use crate::controller::Event;
use crate::{hotkey, paste, tray};
use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

static SHARED: OnceLock<Arc<App>> = OnceLock::new();

/// Makes the app reachable from Win32 window procedures, which take no context.
pub fn install(app: Arc<App>) {
    let _ = SHARED.set(app);
}

pub fn shared() -> &'static Arc<App> {
    SHARED
        .get()
        .expect("the app is installed before any window exists")
}

#[derive(Clone)]
pub struct Settings {
    pub config: Config,
    pub token: String,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Server {
    Checking,
    Ready,
    Unavailable(String),
}

pub struct Notice {
    pub text: String,
    pub error: bool,
    pub until: Instant,
}

/// The latest finished take, for the Dictation page and Copy last message.
#[derive(Clone)]
pub struct Dictation {
    pub text: String,
    /// What became of it, such as "Pasted at your cursor".
    pub outcome: String,
    pub seconds: f64,
    /// From Test microphone, so nothing was pasted.
    pub test: bool,
}

pub struct Status {
    pub server: Server,
    pub recording: bool,
    /// The current recording is a microphone test, which never pastes.
    pub testing: bool,
    pub processing: usize,
    /// Test takes recording or transcribing, for the Microphone page.
    pub tests: usize,
    pub notice: Option<Notice>,
    pub last: Option<Dictation>,
    /// Why the latest take failed. Cleared when the next one starts.
    pub error: Option<String>,
    /// When the microphone opened for the current take. `None` while it starts.
    pub capturing_since: Option<Instant>,
    /// When the last transcript was pasted, for the recording pill.
    pub pasted_at: Option<Instant>,
}

pub struct App {
    settings: Mutex<Settings>,
    status: Mutex<Status>,
    pub events: Sender<Event>,
    wake_monitor: Sender<()>,
    pub deliveries: Arc<Deliveries>,
}

impl App {
    pub fn new(settings: Settings, events: Sender<Event>, wake_monitor: Sender<()>) -> App {
        App {
            settings: Mutex::new(settings),
            status: Mutex::new(Status {
                server: Server::Checking,
                recording: false,
                testing: false,
                processing: 0,
                tests: 0,
                notice: None,
                last: None,
                error: None,
                capturing_since: None,
                pasted_at: None,
            }),
            events,
            wake_monitor,
            deliveries: Arc::new(Deliveries::default()),
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    pub fn apply_settings(&self, settings: Settings) {
        hotkey::set_key(hotkey::key(&settings.config.hotkey).vk);
        *lock(&self.settings) = settings;
        self.recheck();
    }

    /// Saves a change to this PC's settings that does not affect the server
    /// connection, and applies it straight away.
    pub fn change_config(&self, change: impl FnOnce(&mut Config)) -> Result<(), String> {
        let mut settings = lock(&self.settings);
        let mut config = settings.config.clone();
        change(&mut config);
        config
            .save()
            .map_err(|error| format!("Could not save settings: {error}"))?;
        hotkey::set_key(hotkey::key(&config.hotkey).vk);
        settings.config = config;
        drop(settings);
        tray::refresh();
        Ok(())
    }

    /// Checks the server again now, instead of waiting for the next poll.
    pub fn recheck(&self) {
        self.update(|status| status.server = Server::Checking);
        let _ = self.wake_monitor.send(());
    }

    pub fn status<R>(&self, read: impl FnOnce(&Status) -> R) -> R {
        read(&lock(&self.status))
    }

    fn update(&self, change: impl FnOnce(&mut Status)) {
        change(&mut lock(&self.status));
        tray::refresh();
    }

    pub fn set_recording(&self, recording: bool, test: bool) {
        self.update(|status| {
            status.recording = recording;
            status.testing = recording && test;
            status.capturing_since = None;
            if recording {
                status.error = None;
            }
        });
    }

    /// The microphone is open: the take has moved from starting to recording.
    pub fn set_capturing(&self) {
        self.update(|status| status.capturing_since = Some(Instant::now()));
    }

    pub fn set_pasted(&self) {
        self.update(|status| status.pasted_at = Some(Instant::now()));
    }

    /// Counts a take as processing until the returned guard drops.
    pub fn processing(self: &Arc<Self>) -> Processing {
        self.update(|status| status.processing += 1);
        Processing(self.clone())
    }

    /// An informational tooltip message, such as "No speech detected".
    pub fn note(&self, text: &str) {
        log!("note: {text}");
        self.notice(text, false, Duration::from_secs(5));
    }

    /// A take failed: shown in the tooltip and as a notification.
    pub fn fail(&self, text: &str) {
        log!("error: {text}");
        lock(&self.status).error = Some(text.to_string());
        self.notice(text, true, Duration::from_secs(10));
        tray::notify_error();
    }

    fn notice(&self, text: &str, error: bool, lifetime: Duration) {
        let notice = Notice {
            text: text.to_string(),
            error,
            until: Instant::now() + lifetime,
        };
        self.update(|status| status.notice = Some(notice));
    }

    pub fn set_last(&self, dictation: Dictation) {
        self.update(|status| status.last = Some(dictation));
    }

    /// Copies the last dictation, from the tray menu or the Dictation page.
    pub fn copy_last(&self) {
        let Some(text) = self
            .status(|status| status.last.as_ref().map(|last| last.text.clone()))
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        match paste::copy(&text) {
            Ok(()) => {
                self.update(|status| {
                    if let Some(last) = &mut status.last {
                        last.outcome = "Copied to clipboard".into();
                    }
                });
                self.note("Copied the last message.");
            }
            Err(message) => self.fail(&message),
        }
    }

    /// Admission check before a take. Uses the monitor's last answer when the
    /// server was ready, and asks again otherwise.
    pub fn ensure_ready(&self, settings: &Settings) -> Result<(), String> {
        if self.status(|status| status.server == Server::Ready) {
            return Ok(());
        }
        let result = probe(&settings.config.endpoint, &settings.token);
        self.set_server(match &result {
            Ok(_) => Server::Ready,
            Err(message) => Server::Unavailable(message.clone()),
        });
        result.map(drop)
    }

    /// A request failed in a way that suggests the server is gone.
    pub fn server_lost(&self, message: &str) {
        self.set_server(Server::Unavailable(message.to_string()));
        let _ = self.wake_monitor.send(());
    }

    fn set_server(&self, server: Server) {
        let changed = self.status(|status| status.server != server);
        if changed {
            match &server {
                Server::Ready => log!("server: ready"),
                Server::Unavailable(message) => log!("server: unavailable: {message}"),
                Server::Checking => {}
            }
            self.update(|status| status.server = server);
        }
    }
}

/// Counts a test take as running, from its start until its result is shown.
pub struct TestTake(Arc<App>);

impl App {
    pub fn test_take(self: &Arc<Self>) -> TestTake {
        self.update(|status| status.tests += 1);
        TestTake(self.clone())
    }
}

impl Drop for TestTake {
    fn drop(&mut self) {
        self.0.update(|status| status.tests -= 1);
    }
}

pub struct Processing(Arc<App>);

impl Drop for Processing {
    fn drop(&mut self) {
        self.0.update(|status| status.processing -= 1);
    }
}

/// Health, API version and token in one check. Returns the server's message.
pub fn probe(endpoint: &str, token: &str) -> Result<String, String> {
    let api = Api::new(endpoint, token)?;
    let health = api.health().map_err(|error| error.to_string())?;
    if health.api_version != API_VERSION {
        return Err(format!(
            "This server uses API version {}, but Inlay for Windows needs version {API_VERSION}. Update the older side.",
            health.api_version
        ));
    }
    api.check_token().map_err(|error| error.to_string())?;
    if !health.ready {
        return Err(health
            .message
            .unwrap_or_else(|| "The server is not ready yet.".into()));
    }
    Ok(health.message.unwrap_or_else(|| "Server ready.".into()))
}

/// Keeps the tray's server state current. Checks quickly while the server is
/// unavailable and slowly once it is ready. Settings changes wake it early.
pub fn monitor(app: Arc<App>, wake: Receiver<()>) {
    loop {
        let settings = app.settings();
        app.set_server(match probe(&settings.config.endpoint, &settings.token) {
            Ok(_) => Server::Ready,
            Err(message) => Server::Unavailable(message),
        });
        let ready = app.status(|status| status.server == Server::Ready);
        let pause = Duration::from_secs(if ready { 30 } else { 5 });
        if let Err(RecvTimeoutError::Disconnected) = wake.recv_timeout(pause) {
            return;
        }
        while wake.try_recv().is_ok() {}
    }
}

/// Pastes happen in the order takes were released, even when a later take
/// finishes processing first.
#[derive(Default)]
pub struct Deliveries {
    queue: Mutex<Queue>,
    turn: Condvar,
}

#[derive(Default)]
struct Queue {
    issued: u64,
    next: u64,
    done: BTreeSet<u64>,
}

impl Deliveries {
    pub fn issue(self: &Arc<Self>) -> Ticket {
        let mut queue = lock(&self.queue);
        let number = queue.issued;
        queue.issued += 1;
        Ticket {
            number,
            deliveries: self.clone(),
        }
    }
}

/// Holds a place in the paste order. Dropping it, delivered or not, lets the next take go.
pub struct Ticket {
    number: u64,
    deliveries: Arc<Deliveries>,
}

impl Ticket {
    pub fn wait_turn(&self) {
        let mut queue = lock(&self.deliveries.queue);
        while queue.next < self.number {
            queue = self
                .deliveries
                .turn
                .wait(queue)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut queue = lock(&self.deliveries.queue);
        queue.done.insert(self.number);
        loop {
            let next = queue.next;
            if !queue.done.remove(&next) {
                break;
            }
            queue.next += 1;
        }
        self.deliveries.turn.notify_all();
    }
}

/// A panicking take must not take the whole tray down with a poisoned lock.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
