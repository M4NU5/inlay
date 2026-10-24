//! State shared by the tray, the controller and the take threads.

use crate::api::{API_VERSION, Api, ApiError};
use crate::config::Config;
use crate::controller::Event;
use crate::{credentials, hotkey, paste, settings as window, tray};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

static SHARED: OnceLock<Arc<App>> = OnceLock::new();
/// How many shortcut check events stay on screen. Matches the Mac client.
const CHECK_LINES: usize = 16;

thread_local! {
    /// The last notice raised on this thread. Each take runs on its own
    /// thread, so a test's failure reason can't be mixed up with another take's.
    static LAST_NOTICE: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn foreground_is_ours() -> bool {
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut process) };
    process == unsafe { GetCurrentProcessId() }
}

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
}

pub struct Status {
    pub server: Server,
    pub recording: bool,
    /// The take number that set `recording`. Only it may clear it.
    pub recording_take: u64,
    /// The current recording is a microphone test, which never pastes.
    pub testing: bool,
    pub processing: usize,
    /// Test takes recording or transcribing, for the Microphone page.
    pub tests: usize,
    /// Takes finished so far, so History knows to load again.
    pub finished: u64,
    /// The latest test's transcript, or why it stopped without one.
    pub test: Option<Result<Dictation, String>>,
    /// The take number of the latest test. Only it may set `test`.
    pub latest_test: u64,
    pub notice: Option<Notice>,
    pub last: Option<Dictation>,
    /// Why the latest take failed. Cleared when the next one starts.
    pub error: Option<String>,
    /// When the microphone opened for the current take. `None` while it starts.
    pub capturing_since: Option<Instant>,
    /// When and how the last transcript went out, for the recording pill.
    pub delivered: Option<(Instant, paste::Delivered)>,
    /// When the running shortcut check started.
    pub checking: Option<Instant>,
    /// The latest shortcut check's events, oldest first. Kept in memory only.
    pub check_lines: Vec<String>,
}

/// Why the server can't take a dictation.
pub struct Unready {
    pub message: String,
    /// The credential was refused. Asking again soon would only count as more
    /// failed sign-ins, and the server locks an address out after ten.
    pub credential: bool,
}

impl Unready {
    fn new(message: impl Into<String>) -> Unready {
        Unready {
            message: message.into(),
            credential: false,
        }
    }
}

impl From<ApiError> for Unready {
    fn from(error: ApiError) -> Unready {
        Unready {
            credential: error.is_credential(),
            message: error.to_string(),
        }
    }
}

pub struct App {
    settings: Mutex<Settings>,
    /// Held while enrolling. Enrolling again rotates the credential, so two
    /// checks enrolling at once would leave this PC holding a dead one.
    enrolling: Mutex<()>,
    status: Mutex<Status>,
    pub events: Sender<Event>,
    wake_monitor: Sender<()>,
    pub deliveries: Arc<Deliveries>,
}

impl App {
    pub fn new(settings: Settings, events: Sender<Event>, wake_monitor: Sender<()>) -> App {
        App {
            settings: Mutex::new(settings),
            enrolling: Mutex::new(()),
            status: Mutex::new(Status {
                server: Server::Checking,
                recording: false,
                recording_take: 0,
                testing: false,
                processing: 0,
                tests: 0,
                test: None,
                latest_test: 0,
                finished: 0,
                notice: None,
                last: None,
                error: None,
                capturing_since: None,
                delivered: None,
                checking: None,
                check_lines: Vec::new(),
            }),
            events,
            wake_monitor,
            deliveries: Arc::new(Deliveries::default()),
        }
    }

    pub fn settings(&self) -> Settings {
        lock(&self.settings).clone()
    }

    /// Saves and applies a new connection: the token in Credential Manager,
    /// the rest in the config file. Holds the settings while it does, so an
    /// enrollment finishing meanwhile can't overwrite the token just entered.
    pub fn save_connection(&self, connection: Settings) -> Result<(), String> {
        {
            let mut settings = lock(&self.settings);
            credentials::write(&connection.config.endpoint, &connection.token)?;
            connection
                .config
                .save()
                .map_err(|error| format!("Could not save settings: {error}"))?;
            hotkey::apply(&connection.config);
            *settings = connection;
        }
        self.recheck();
        Ok(())
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
        hotkey::apply(&config);
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

    /// A take that ends after a newer one started leaves the newer one's
    /// status alone.
    pub fn set_recording(&self, take: u64, recording: bool, test: bool) {
        self.update(|status| {
            if !recording && status.recording_take != take {
                return;
            }
            status.recording_take = take;
            status.recording = recording;
            status.testing = recording && test;
            status.capturing_since = None;
            if recording {
                status.error = None;
                // A new take replaces the last one's tick.
                status.delivered = None;
            }
        });
    }

    /// The microphone is open: the take has moved from starting to recording.
    pub fn set_capturing(&self, take: u64) {
        self.update(|status| {
            if status.recording && status.recording_take == take {
                status.capturing_since = Some(Instant::now());
            }
        });
    }

    pub fn set_delivered(&self, delivered: paste::Delivered) {
        self.update(|status| status.delivered = Some((Instant::now(), delivered)));
    }

    /// Counts a take as processing until the returned guard drops.
    pub fn processing(self: &Arc<Self>) -> Processing {
        self.update(|status| status.processing += 1);
        Processing(self.clone())
    }

    /// An informational tooltip message, such as "No speech detected".
    /// Starts a shortcut check's list of events afresh.
    pub fn check_started(&self, first: &str) {
        self.update(|status| {
            status.checking = Some(Instant::now());
            status.check_lines.clear();
        });
        self.check_line(first);
    }

    /// Adds a line to the running shortcut check, as on the Mac: the time
    /// since it started, and whether Inlay or another app was in front.
    pub fn check_line(&self, text: &str) {
        let front = if foreground_is_ours() {
            "Inlay"
        } else {
            "background"
        };
        self.update(|status| {
            if let Some(started) = status.checking {
                let seconds = started.elapsed().as_secs_f64();
                status
                    .check_lines
                    .push(format!("{seconds:.2}s [{front}] {text}"));
                let extra = status.check_lines.len().saturating_sub(CHECK_LINES);
                status.check_lines.drain(..extra);
            }
        });
    }

    pub fn check_ended(&self) {
        self.check_line("Check ended. No audio was recorded.");
        self.update(|status| status.checking = None);
    }

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
        LAST_NOTICE.with(|last| *last.borrow_mut() = Some(text.to_string()));
        let notice = Notice {
            text: text.to_string(),
            error,
            until: Instant::now() + lifetime,
        };
        self.update(|status| status.notice = Some(notice));
    }

    pub fn set_last(&self, dictation: Dictation) {
        self.update(|status| {
            status.last = Some(dictation);
            status.finished += 1;
        });
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
    /// server was ready, and asks again otherwise. Returns the settings the take
    /// should use: asking again may have enrolled, replacing the admin token.
    pub fn ensure_ready(&self, settings: Settings) -> Result<Settings, String> {
        if !self.status(|status| status.server == Server::Ready) {
            let result = self.probe(&settings);
            self.set_server(match &result {
                Ok(_) => Server::Ready,
                Err(unready) => Server::Unavailable(unready.message.clone()),
            });
            result.map_err(|unready| unready.message)?;
        }
        // The monitor may have enrolled since `settings` was read, too.
        let current = self.settings();
        // Only the token can have changed for the server just checked.
        Ok(if current.config.endpoint == settings.config.endpoint {
            Settings {
                token: current.token,
                ..settings
            }
        } else {
            settings
        })
    }

    /// Health, API version and credential in one check. Returns the server's
    /// message. A server's admin token is first swapped for this PC's own
    /// credential, as the Mac does.
    pub fn probe(&self, settings: &Settings) -> Result<String, Unready> {
        let api = Api::new(&settings.config.endpoint, &settings.token).map_err(Unready::new)?;
        if settings.token.is_empty() {
            // Only liveness answers without a credential.
            api.liveness()?;
            return Err(Unready {
                message: "Paste the server's admin token on This PC, then Connect.".into(),
                credential: true,
            });
        }
        let health = api.health()?;
        if health.api_version != API_VERSION {
            return Err(Unready::new(format!(
                "This server uses API version {}, but Inlay for Windows needs version {API_VERSION}. Update the older side.",
                health.api_version
            )));
        }
        match api.check_token() {
            Err(error) if error.code() == Some("enrollment_required") => {
                let token = self.enroll(settings)?;
                Api::new(&settings.config.endpoint, &token)
                    .map_err(Unready::new)?
                    .check_token()?;
            }
            result => result?,
        }
        if !health.ready {
            return Err(Unready::new(
                health
                    .message
                    .unwrap_or_else(|| "The server is not ready yet.".into()),
            ));
        }
        Ok(health.message.unwrap_or_else(|| "Server ready.".into()))
    }

    /// Enrolls this PC with the admin token in `settings`, and keeps the
    /// credential it gets back in place of that token. Returns the token to
    /// use now.
    fn enroll(&self, settings: &Settings) -> Result<String, Unready> {
        let _one = lock(&self.enrolling);
        let current = self.settings();
        if current.config.endpoint != settings.config.endpoint {
            return Err(Unready::new(
                "The server address changed. Connecting again…",
            ));
        }
        if current.token != settings.token {
            // Another check enrolled while this one waited, or Connect saved a new token.
            return Ok(current.token);
        }
        let token = Api::new(&settings.config.endpoint, &settings.token)
            .map_err(Unready::new)?
            .enroll(&settings.config.device_id, &settings.config.device_name)?;
        {
            let mut saved = lock(&self.settings);
            if saved.config.endpoint != settings.config.endpoint || saved.token != settings.token {
                return Err(Unready::new("The connection changed. Connecting again…"));
            }
            credentials::write(&settings.config.endpoint, &token).map_err(Unready::new)?;
            saved.token = token.clone();
        }
        log!("server: enrolled this PC and saved its own credential");
        window::token_replaced();
        Ok(token)
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

/// Counts test take `take` as running, from its start until its result is
/// shown. A test that ends without a transcript keeps the reason it gave. An
/// earlier test still transcribing never replaces a later one's result.
pub struct TestTake(Arc<App>, u64);

impl App {
    pub fn test_take(self: &Arc<Self>, take: u64) -> TestTake {
        self.update(|status| {
            status.tests += 1;
            status.test = None;
            status.latest_test = take;
        });
        TestTake(self.clone(), take)
    }

    pub fn set_test_result(&self, take: u64, dictation: Dictation) {
        self.update(|status| {
            if status.latest_test == take {
                status.test = Some(Ok(dictation));
            }
        });
    }
}

impl Drop for TestTake {
    fn drop(&mut self) {
        // Dropped on the take's own thread, so this is the test's own reason.
        let reason = LAST_NOTICE
            .with(|last| last.borrow_mut().take())
            .unwrap_or_else(|| "The test stopped before it was transcribed.".into());
        let take = self.1;
        self.0.update(|status| {
            status.tests -= 1;
            if status.latest_test == take && status.test.is_none() {
                status.test = Some(Err(reason));
            }
        });
    }
}

pub struct Processing(Arc<App>);

impl Drop for Processing {
    fn drop(&mut self) {
        self.0.update(|status| status.processing -= 1);
    }
}

/// Keeps the tray's server state current. Checks quickly while the server is
/// unavailable and slowly once it is ready, or once it has refused the
/// credential. Settings changes wake it early.
pub fn monitor(app: Arc<App>, wake: Receiver<()>) {
    loop {
        let settings = app.settings();
        let pause = match app.probe(&settings) {
            Ok(_) => {
                app.set_server(Server::Ready);
                30
            }
            Err(unready) => {
                app.set_server(Server::Unavailable(unready.message));
                if unready.credential { 60 } else { 5 }
            }
        };
        let pause = Duration::from_secs(pause);
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
