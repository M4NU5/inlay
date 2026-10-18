//! One dictation take, following the Mac client's flow: admission, capture with
//! acknowledged uploads while recording, finish with exact frame counts, NDJSON
//! progress, an ordered paste of `insertionText`, then the delivery receipt.

use crate::api::{Api, ApiError, Generation};
use crate::app::{App, Ticket};
use crate::audio::{self, SAMPLE_RATE, Stop};
use crate::controller::Event;
use crate::{hotkey, paste};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The server rejects recordings shorter than 0.25 seconds.
const MIN_FRAMES: u64 = SAMPLE_RATE as u64 / 4;
/// About half a second of audio per upload, as on the Mac. Far below the 1 MiB chunk limit.
const CHUNK_BYTES: usize = 32_000;
/// Roughly five seconds of microphone callbacks may wait for the network.
const BACKLOG_CHUNKS: usize = 512;

enum Signal {
    Release,
    Limit,
    Cancel(String),
    Failed(String),
}

pub struct Handle {
    pub id: u64,
    signals: Sender<Signal>,
    thread: JoinHandle<()>,
}

impl Handle {
    pub fn release(&self) {
        let _ = self.signals.send(Signal::Release);
    }

    pub fn cancel(&self, reason: &str) {
        let _ = self.signals.send(Signal::Cancel(reason.into()));
    }

    /// Lets a cancelled take tell the server before the process exits.
    pub fn cancel_and_wait(self, reason: &str) {
        self.cancel(reason);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

pub fn start(id: u64, app: Arc<App>) -> Handle {
    let (signals, inbox) = mpsc::channel();
    let own = signals.clone();
    let thread = thread::Builder::new()
        .name(format!("take-{id}"))
        .spawn(move || {
            app.set_recording(true);
            let released = record(&app, &inbox, &own);
            app.set_recording(false);
            let _ = app.events.send(Event::TakeEnded(id));
            if let Some(released) = released {
                process(&app, released);
            }
        })
        .expect("cannot start a take thread");
    Handle {
        id,
        signals,
        thread,
    }
}

/// A take whose microphone has stopped. Its uploads may still be draining.
struct Released {
    api: Api,
    generation: String,
    uploader: JoinHandle<Result<u64, ApiError>>,
    keep_original: bool,
    ticket: Ticket,
}

fn record(app: &Arc<App>, inbox: &Receiver<Signal>, own: &Sender<Signal>) -> Option<Released> {
    let settings = app.settings();
    let api = match Api::new(&settings.config.endpoint, &settings.token) {
        Ok(api) => api,
        Err(message) => {
            app.fail(&message);
            return None;
        }
    };
    // Admission comes before the microphone: an unavailable server means no recording.
    if let Err(message) = app.ensure_ready(&settings) {
        app.fail(&format!("Inlay can't record. {message}"));
        return None;
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let created = match api.create(
        &request_id,
        &settings.config.device_id,
        &settings.config.device_name,
    ) {
        Ok(created) => created,
        Err(error) => {
            if matches!(error, ApiError::Offline(_)) {
                app.server_lost(&error.to_string());
            }
            app.fail(&format!("Inlay can't record. {error}"));
            return None;
        }
    };
    let generation = created.id.clone();
    log!("take: admitted generation {generation}");
    if created.status != "receiving" {
        cancel_quietly(&api, &generation);
        app.fail("The server did not accept the recording.");
        return None;
    }
    // A hold released during admission is too short to keep.
    if let Ok(signal) = inbox.try_recv() {
        cancel_quietly(&api, &generation);
        stopped_early(app, signal);
        return None;
    }

    let keep_original = created.settings.preferences.keep_original_audio;
    let (chunks, received) = mpsc::sync_channel(BACKLOG_CHUNKS);
    let uploader = {
        let (api, generation, own) = (api.clone(), generation.clone(), own.clone());
        thread::spawn(move || {
            let result = upload(&api, &generation, received, keep_original);
            if let Err(error) = &result {
                let _ = own.send(Signal::Failed(error.to_string()));
            }
            result
        })
    };
    let notify: audio::Notify = {
        let own = own.clone();
        Arc::new(move |stop| {
            let _ = own.send(match stop {
                Stop::Limit => Signal::Limit,
                Stop::Backlog => {
                    Signal::Failed("The connection cannot keep up with the microphone. This recording was stopped.".into())
                }
                Stop::Device(error) => Signal::Failed(format!("The microphone stopped: {error}")),
            });
        })
    };
    let recording = match audio::start(settings.config.microphone.as_deref(), chunks, notify) {
        Ok(recording) => recording,
        Err(message) => {
            cancel_quietly(&api, &generation);
            let _ = uploader.join();
            app.fail(&message);
            return None;
        }
    };
    log!("take: recording from {}", recording.device);

    let signal = inbox
        .recv()
        .unwrap_or(Signal::Cancel("Recording stopped.".into()));
    let frames = recording.stop();
    match signal {
        Signal::Release => {}
        Signal::Limit => app.note("Reached the 3-minute limit. Transcribing what was recorded."),
        other => {
            cancel_quietly(&api, &generation);
            let _ = uploader.join();
            stopped_early(app, other);
            return None;
        }
    }
    if frames < MIN_FRAMES {
        cancel_quietly(&api, &generation);
        let _ = uploader.join();
        app.note("Too short to transcribe. Hold the key while you speak.");
        return None;
    }
    log!(
        "take: released after {:.2} s",
        frames as f64 / f64::from(SAMPLE_RATE)
    );
    Some(Released {
        api,
        generation,
        uploader,
        keep_original,
        ticket: app.deliveries.issue(),
    })
}

fn stopped_early(app: &App, signal: Signal) {
    match signal {
        Signal::Failed(message) => app.fail(&message),
        Signal::Cancel(reason) => app.note(&reason),
        Signal::Release | Signal::Limit => {
            app.note("Too short to transcribe. Hold the key while you speak.")
        }
    }
}

fn cancel_quietly(api: &Api, generation: &str) {
    if let Err(error) = api.cancel(generation) {
        log!("take: could not cancel {generation}: {error}");
    }
}

/// Uploads while recording, waiting for each acknowledgement. With original
/// audio kept, the same 16 kHz mono audio doubles as the `original` stream,
/// so both streams always cover the same interval.
fn upload(
    api: &Api,
    generation: &str,
    received: Receiver<Vec<f32>>,
    keep_original: bool,
) -> Result<u64, ApiError> {
    let mut pending: Vec<u8> = Vec::with_capacity(CHUNK_BYTES * 2);
    let mut sequence = 0;
    let mut frames = 0;
    let send = |pending: &mut Vec<u8>, sequence: &mut u64, frames: u64| -> Result<(), ApiError> {
        api.upload(generation, "inference", *sequence, frames, pending)?;
        if keep_original {
            api.upload(generation, "original", *sequence, frames, pending)?;
        }
        *sequence += 1;
        pending.clear();
        Ok(())
    };
    for chunk in received {
        frames += chunk.len() as u64;
        pending.extend(chunk.iter().flat_map(|sample| sample.to_le_bytes()));
        if pending.len() >= CHUNK_BYTES {
            send(&mut pending, &mut sequence, frames)?;
        }
    }
    if !pending.is_empty() {
        send(&mut pending, &mut sequence, frames)?;
    }
    log!("take: uploaded {frames} frames in {sequence} chunk(s) per stream");
    Ok(frames)
}

fn process(app: &Arc<App>, released: Released) {
    let _busy = app.processing();
    let Released {
        api,
        generation,
        uploader,
        keep_original,
        ticket,
    } = released;
    let frames = match uploader.join() {
        Ok(Ok(frames)) => frames,
        Ok(Err(error)) => {
            cancel_quietly(&api, &generation);
            app.fail(&error.to_string());
            return;
        }
        Err(_) => {
            cancel_quietly(&api, &generation);
            app.fail("The upload stopped unexpectedly.");
            return;
        }
    };
    let finished = match api.finish(&generation, frames, keep_original.then_some(frames)) {
        Ok(record) => record,
        Err(error) => {
            // Only a definite rejection proves the server did not seal it.
            if error.is_definite_rejection() {
                cancel_quietly(&api, &generation);
            }
            app.fail(&error.to_string());
            return;
        }
    };
    let mut last_status = String::new();
    let result = if finished.is_terminal() {
        Ok(finished)
    } else {
        api.events(&generation, |record| {
            if record.status != last_status {
                log!("take: {generation} is {}", record.status);
                last_status = record.status.clone();
            }
        })
    };
    let record = match result {
        Ok(record) if record.status == "completed" => record,
        Ok(record) => {
            app.fail(
                record
                    .error
                    .as_deref()
                    .unwrap_or("The server could not process this recording."),
            );
            return;
        }
        Err(error) => {
            app.fail(&error.to_string());
            return;
        }
    };
    ticket.wait_turn();
    // Never paste while a hold is in progress: the held key would join the chord.
    while hotkey::is_held() || app.status(|status| status.recording) {
        thread::sleep(Duration::from_millis(50));
    }
    let (status, message) = deliver(app, &record);
    drop(ticket);
    log!("take: {generation} delivery {status}");
    if let Err(error) = api.delivery(&generation, status, &message) {
        log!("take: could not report delivery for {generation}: {error}");
    }
}

/// Returns the receipt status and message, using the Swift client's statuses.
fn deliver(app: &App, record: &Generation) -> (&'static str, String) {
    if record.insertion_text.is_empty() {
        if record.preview_text.is_empty() && record.continuation.is_none() {
            app.note("No speech detected.");
            return ("none", "No speech detected".into());
        }
        app.note("Nothing new to paste.");
        return ("unconfirmed", "Nothing new to paste.".into());
    }
    app.set_last_transcript(&record.insertion_text);
    log!(
        "take: pasting {} characters",
        record.insertion_text.chars().count()
    );
    match paste::paste(&record.insertion_text) {
        Ok(()) => {
            app.note("Pasted. If nothing appeared, use Copy last transcript.");
            (
                "unconfirmed",
                "Pasted with Ctrl+V. Windows cannot confirm insertion.".into(),
            )
        }
        Err(message) => {
            app.fail(&format!(
                "{message} Use Copy last transcript from the tray menu."
            ));
            ("failed", message)
        }
    }
}
