//! Takes this PC still owes the user, as on the Mac. A take is saved in
//! `pending-generations.json` beside the config from the moment it is sealed
//! until it is delivered. If Inlay quits, or loses the server, before then, the
//! result is found later and offered for copying. It is never pasted: the
//! cursor has moved on. Only IDs and frame counts are saved, never text,
//! tokens or where the text was going.

use crate::api::{Api, ApiError, Generation};
use crate::audio::SAMPLE_RATE;
use crate::{config, tray};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE: &str = "pending-generations.json";
/// Takes older than this are dropped unchecked, as on the Mac.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    endpoint: String,
    /// What the take was sealed with, to seal it again if the server never
    /// got the first one.
    inference_frames: u64,
    original_frames: Option<u64>,
    /// Seconds since 1970.
    sealed_at: u64,
}

/// A result that finished with no take left to deliver it.
#[derive(Clone)]
pub struct Finished {
    pub id: String,
    pub text: String,
    pub seconds: f64,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    /// Takes this run is still delivering itself.
    live: HashSet<String>,
    finished: Vec<Finished>,
}

pub struct Pending {
    state: Mutex<State>,
}

impl Pending {
    /// An unreadable file means no pending takes.
    pub fn load() -> Arc<Pending> {
        let entries = fs::read(config::data_dir().join(FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Pending {
            state: Mutex::new(State {
                entries,
                ..State::default()
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Saves a take that is about to be sealed. This run delivers it itself
    /// until the returned guard drops, which forgets it unless `keep` was called.
    pub fn track(
        self: &Arc<Self>,
        id: &str,
        endpoint: &str,
        inference_frames: u64,
        original_frames: Option<u64>,
    ) -> Tracked {
        let mut state = self.state();
        state.entries.retain(|entry| entry.id != id);
        state.entries.push(Entry {
            id: id.into(),
            endpoint: endpoint.into(),
            inference_frames,
            original_frames,
            sealed_at: now(),
        });
        state.live.insert(id.into());
        save(&state.entries);
        Tracked {
            pending: self.clone(),
            id: id.into(),
            keep: false,
        }
    }

    pub fn forget(&self, id: &str) {
        self.forget_where(|entry| entry.id == id);
    }

    /// The connection moved to `endpoint`. Takes owed by another server can't
    /// be checked any more. A new token for the same server keeps them.
    pub fn keep_only(&self, endpoint: &str) {
        self.forget_where(|entry| entry.endpoint != endpoint);
    }

    fn forget_where(&self, unwanted: impl Fn(&Entry) -> bool) {
        let mut state = self.state();
        let count = state.entries.len();
        state.entries.retain(|entry| !unwanted(entry));
        if state.entries.len() == count {
            return;
        }
        save(&state.entries);
        let State {
            entries, finished, ..
        } = &mut *state;
        let shown = finished.len();
        finished.retain(|result| entries.iter().any(|entry| entry.id == result.id));
        if finished.len() != shown {
            drop(state);
            tray::refresh();
        }
    }

    /// The oldest result waiting to be copied.
    pub fn finished(&self) -> Option<Finished> {
        self.state().finished.first().cloned()
    }

    /// Asks the server about every take this run isn't delivering itself:
    /// offers finished ones for copying, seals any the server never got the
    /// seal for, and forgets the rest once they are settled.
    pub fn reconcile(&self, api: &Api, endpoint: &str) {
        let cutoff = now().saturating_sub(KEEP.as_secs());
        self.forget_where(|entry| entry.sealed_at < cutoff);
        let due: Vec<Entry> = {
            let state = self.state();
            state
                .entries
                .iter()
                .filter(|entry| entry.endpoint == endpoint && !state.live.contains(&entry.id))
                .cloned()
                .collect()
        };
        for entry in due {
            let record = match api.generation(&entry.id) {
                Ok(record) => record,
                Err(ApiError::Rejected { status: 404, .. }) => {
                    log!("pending: {} is gone from the server", entry.id);
                    self.forget(&entry.id);
                    continue;
                }
                Err(error) => {
                    log!("pending: could not check {}: {error}", entry.id);
                    continue;
                }
            };
            match record.status.as_str() {
                "completed" if record.delivery.is_none() && !record.final_text.is_empty() => {
                    self.offer(&entry, &record)
                }
                "completed" | "failed" | "cancelled" => {
                    log!("pending: {} is {}", entry.id, record.status);
                    self.forget(&entry.id);
                }
                "receiving" => self.seal_again(api, &entry),
                _ => {}
            }
        }
    }

    fn offer(&self, entry: &Entry, record: &Generation) {
        let mut state = self.state();
        // Copied or forgotten while the server was asked.
        let tracked = state.entries.iter().any(|saved| saved.id == entry.id);
        if !tracked || state.finished.iter().any(|result| result.id == entry.id) {
            return;
        }
        log!("pending: {} finished undelivered", entry.id);
        state.finished.push(Finished {
            id: entry.id.clone(),
            text: record.final_text.clone(),
            seconds: entry.inference_frames as f64 / f64::from(SAMPLE_RATE),
        });
        drop(state);
        tray::refresh();
    }

    fn seal_again(&self, api: &Api, entry: &Entry) {
        match api.finish(&entry.id, entry.inference_frames, entry.original_frames) {
            Ok(_) => log!("pending: sealed {} again", entry.id),
            // A refused credential may have just been replaced, so it doesn't
            // prove the speech is lost.
            Err(ApiError::Rejected { status, .. })
                if (400..500).contains(&status) && !matches!(status, 401 | 403 | 408 | 429) =>
            {
                log!(
                    "pending: the server refused to seal {} ({status})",
                    entry.id
                );
                self.forget(&entry.id);
            }
            Err(error) => log!("pending: could not seal {} again: {error}", entry.id),
        }
    }
}

/// A take this run is delivering. Dropping it forgets the take.
pub struct Tracked {
    pending: Arc<Pending>,
    id: String,
    keep: bool,
}

impl Tracked {
    /// The result may still arrive, so leave the take for `reconcile`.
    pub fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.pending.state().live.remove(&self.id);
        if !self.keep {
            self.pending.forget(&self.id);
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn save(entries: &[Entry]) {
    let directory = config::data_dir();
    let result = fs::create_dir_all(&directory).and_then(|()| {
        let temporary = directory.join(format!("{FILE}.tmp"));
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(entries).map_err(std::io::Error::other)?,
        )?;
        fs::rename(&temporary, directory.join(FILE))
    });
    if let Err(error) = result {
        log!("pending: could not save {FILE}: {error}");
    }
}
