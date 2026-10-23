//! Blocking client for the Inlay v1 HTTP API (`Server/api/openapi.yaml`).
//! Each take builds its own `Api`, so editing settings mid-take cannot send its
//! audio or token to a different server.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fmt, thread};

pub const API_VERSION: u64 = 4;
const RESPONSE_LIMIT: u64 = 16 * 1024 * 1024;
const EVENT_LINE_LIMIT: u64 = 2 * 1024 * 1024;
/// A recording's audio is at most 180 seconds; original audio can be stereo
/// at a high rate, so allow well over that.
const ARTIFACT_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub enum ApiError {
    /// No HTTP response: refused, unreachable, timed out, or TLS failed.
    Offline(String),
    /// The server answered with an error status. `code` is the server's
    /// machine-readable reason, such as `enrollment_required`.
    Rejected {
        status: u16,
        code: Option<String>,
        message: String,
    },
    /// The connection closed before the generation finished.
    Disconnected,
    Invalid(String),
}

impl ApiError {
    fn is_transient(&self) -> bool {
        match self {
            ApiError::Offline(_) | ApiError::Disconnected => true,
            ApiError::Rejected { status, .. } => matches!(status, 408 | 429 | 502 | 503 | 504),
            ApiError::Invalid(_) => false,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Rejected { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// The server refused the credential, or is throttling failed ones.
    pub fn is_credential(&self) -> bool {
        match self {
            ApiError::Rejected { status, code, .. } => {
                *status == 401 || code.as_deref() == Some("auth_throttled")
            }
            _ => false,
        }
    }

    /// The server answered without sealing the recording, so cancelling is safe.
    pub fn is_definite_rejection(&self) -> bool {
        matches!(self, ApiError::Rejected { status, .. } if (400..500).contains(status) && !matches!(status, 408 | 429))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApiError::Offline(detail) => {
                write!(formatter, "Cannot reach the Inlay server ({detail}).")
            }
            ApiError::Rejected { message, .. } => formatter.write_str(message),
            ApiError::Disconnected => formatter.write_str(
                "The server connection was interrupted. Any completed result is in shared history.",
            ),
            ApiError::Invalid(detail) => write!(
                formatter,
                "The server returned an invalid response ({detail})."
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub api_version: u64,
    pub ready: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub speech: Option<Runtime>,
    #[serde(default)]
    pub proofreading: Option<Runtime>,
}

/// One of the server's models, as Server preferences shows it.
#[derive(Clone, Deserialize)]
pub struct Runtime {
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub backend: String,
    pub ready: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// The shared preferences and the revision a save must name.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    pub revision: u64,
    pub preferences: SharedPreferences,
}

/// The server's shared preferences. Fields this client doesn't edit, and any
/// a newer server adds, ride along in `other` so a save keeps them.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedPreferences {
    pub language: String,
    #[serde(default = "default_prompt")]
    pub proofreading_prompt: String,
    pub dictionary: Dictionary,
    pub text_correction_enabled: bool,
    pub keep_original_audio: bool,
    #[serde(default = "default_retention")]
    pub audio_retention_days: u32,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

/// The server's built-in cleanup instructions, as the Mac's Reset to default uses.
pub const DEFAULT_PROMPT: &str = include_str!("../../Server/api/default-proofreading-prompt.txt");

fn default_prompt() -> String {
    DEFAULT_PROMPT.into()
}

fn default_retention() -> u32 {
    90
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Dictionary {
    pub lists: Vec<DictionaryList>,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct DictionaryList {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub entries: Vec<DictionaryEntry>,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub id: String,
    pub term: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub is_priority: bool,
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

/// How much recording audio the server holds.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    pub generation_count: u64,
    pub audio_byte_count: u64,
    pub inference_byte_count: u64,
    pub original_byte_count: u64,
    #[serde(default)]
    pub oldest_audio_at: Option<String>,
    pub free_byte_count: u64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Generation {
    pub id: String,
    pub status: String,
    pub settings: Snapshot,
    #[serde(default)]
    pub insertion_text: String,
    #[serde(default)]
    pub preview_text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub continuation: Option<Value>,
}

impl Generation {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed" | "cancelled")
    }
}

#[derive(Clone, Deserialize)]
pub struct Snapshot {
    pub preferences: Preferences,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    #[serde(default = "enabled")]
    pub keep_original_audio: bool,
}

fn enabled() -> bool {
    true
}

/// One row of shared history. The list carries only this; the History page
/// loads the whole record, a [`Detail`], for the row it shows.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub device: Device,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub preview_text: String,
    #[serde(default)]
    pub queue_position: Option<u64>,
    #[serde(default)]
    pub imported_source: Option<Imported>,
}

/// A generation as the History page shows it. Only the fields it reads; the
/// server sends more.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub id: String,
    pub device: Device,
    pub status: String,
    pub created_at: String,
    #[serde(default)]
    pub final_text: String,
    #[serde(default)]
    pub raw_text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub inference_audio: Option<Audio>,
    #[serde(default)]
    pub original_audio: Option<Audio>,
    /// Why there is no original audio, such as the setting being off.
    #[serde(default)]
    pub original_audio_omitted_reason: Option<String>,
    #[serde(default)]
    pub speech: Option<Model>,
    #[serde(default)]
    pub imported_source: Option<Imported>,
}

impl Detail {
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "failed" | "cancelled")
    }

    /// A failed or cancelled take with its audio still on the server.
    pub fn can_transcribe_again(&self) -> bool {
        matches!(self.status.as_str(), "failed" | "cancelled")
            && self.inference_audio.is_some()
            && self.imported_source.is_none()
    }

    /// The recording's length, from its audio or the imported source.
    pub fn seconds(&self) -> f64 {
        match (&self.inference_audio, &self.imported_source) {
            (Some(audio), _) if audio.sample_rate > 0 => {
                audio.frame_count as f64 / audio.sample_rate as f64
            }
            (_, Some(source)) => source.duration_seconds.unwrap_or(0.0),
            _ => 0.0,
        }
    }
}

#[derive(Clone, PartialEq, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Audio {
    pub sample_rate: u64,
    pub frame_count: u64,
}

#[derive(Clone, Deserialize)]
pub struct Model {
    #[serde(rename = "modelID")]
    pub model_id: String,
    pub backend: String,
}

#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    #[serde(default)]
    pub source_status: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
}

/// One page of shared history, newest first.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub items: Vec<Summary>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkReceipt {
    next_sequence: u64,
    frame_count: u64,
}

#[derive(Clone)]
pub struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
}

enum Body<'a> {
    Empty,
    Json(&'a Value),
    Bytes(&'a [u8]),
}

impl Api {
    pub fn new(endpoint: &str, token: &str) -> Result<Api, String> {
        let tls = native_tls::TlsConnector::new()
            .map_err(|error| format!("TLS is unavailable: {error}"))?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(15))
            .timeout_write(Duration::from_secs(15))
            // A redirect is a configuration error. Never follow one with audio or a token.
            .redirects(0)
            .user_agent(concat!("inlay-windows/", env!("CARGO_PKG_VERSION")))
            .tls_connector(Arc::new(tls))
            .build();
        Ok(Api {
            base: endpoint.trim_end_matches('/').to_string(),
            token: token.to_string(),
            agent,
        })
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let request = self
            .agent
            .request(method, &format!("{}/{path}", self.base))
            .set("Accept", "application/json");
        if self.token.is_empty() {
            request
        } else {
            request.set("Authorization", &format!("Bearer {}", self.token))
        }
    }

    /// Reachability only. The one route that needs no credential, so asking
    /// without a token never counts as a failed sign-in.
    pub fn liveness(&self) -> Result<(), ApiError> {
        send(self.request("GET", "v1/liveness"), Body::Empty).map(drop)
    }

    pub fn health(&self) -> Result<Health, ApiError> {
        decode(send(self.request("GET", "v1/health"), Body::Empty)?)
    }

    /// Health also accepts the server's admin token, so a data route is what
    /// proves this is a device credential. The admin token gets 403
    /// `enrollment_required` here.
    pub fn check_token(&self) -> Result<(), ApiError> {
        send(self.request("GET", "v1/preferences"), Body::Empty).map(drop)
    }

    /// Swaps the admin token for this PC's own credential, as the Mac does.
    /// Enrolling again rotates the credential, so the old one stops working.
    pub fn enroll(&self, device_id: &str, device_name: &str) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Enrolled {
            token: String,
        }
        let body = json!({ "device": { "id": device_id, "name": device_name } });
        let enrolled: Enrolled = decode(send(
            self.request("POST", "v1/devices/enroll"),
            Body::Json(&body),
        )?)?;
        Ok(enrolled.token)
    }

    /// Admission happens here, before the microphone opens. `mode` is
    /// `dictation`, or `test` for a microphone test.
    pub fn create(
        &self,
        request_id: &str,
        device_id: &str,
        device_name: &str,
        mode: &str,
    ) -> Result<Generation, ApiError> {
        let body = json!({
            "requestID": request_id,
            "device": { "id": device_id, "name": device_name },
            "mode": mode,
        });
        decode(send(
            self.request("POST", "v1/generations"),
            Body::Json(&body),
        )?)
    }

    /// Uploads one sequenced chunk of mono 16 kHz float32 audio and checks the
    /// acknowledgement. `frames_total` is the stream's length after this chunk.
    pub fn upload(
        &self,
        id: &str,
        kind: &str,
        sequence: u64,
        frames_total: u64,
        bytes: &[u8],
    ) -> Result<(), ApiError> {
        let request = self
            .request("POST", &format!("v1/generations/{id}/audio/{kind}"))
            .query("sequence", &sequence.to_string())
            .query("sampleRate", "16000")
            .query("channels", "1")
            .set("Content-Type", "application/octet-stream");
        let receipt: ChunkReceipt = decode(send(request, Body::Bytes(bytes))?)?;
        if receipt.next_sequence != sequence + 1 || receipt.frame_count != frames_total {
            return Err(ApiError::Invalid(format!(
                "{kind} chunk {sequence} was not acknowledged"
            )));
        }
        Ok(())
    }

    /// Seals the recording. Continuation is never requested: it needs a
    /// confirmed caret anchor, which plain paste cannot provide.
    pub fn finish(
        &self,
        id: &str,
        inference_frames: u64,
        original_frames: Option<u64>,
    ) -> Result<Generation, ApiError> {
        let mut body = json!({ "inferenceFrames": inference_frames });
        if let Some(frames) = original_frames {
            body["originalFrames"] = frames.into();
        }
        let mut failures = 0;
        loop {
            match send(
                self.request("POST", &format!("v1/generations/{id}/finish")),
                Body::Json(&body),
            )
            .and_then(decode)
            {
                // Sealing the same frame counts is idempotent, so a replay recovers a lost reply.
                Err(error) if error.is_transient() && failures < 3 => {
                    failures += 1;
                    thread::sleep(Duration::from_millis(250 * failures));
                }
                result => return result,
            }
        }
    }

    pub fn preferences(&self) -> Result<Shared, ApiError> {
        decode(send(self.request("GET", "v1/preferences"), Body::Empty)?)
    }

    /// Saves `shared.preferences` over revision `shared.revision`. The server
    /// answers 409 when another device saved first.
    pub fn save_preferences(&self, shared: &Shared) -> Result<Shared, ApiError> {
        let body =
            serde_json::to_value(shared).map_err(|error| ApiError::Invalid(error.to_string()))?;
        decode(send(
            self.request("PUT", "v1/preferences"),
            Body::Json(&body),
        )?)
    }

    pub fn storage(&self) -> Result<Storage, ApiError> {
        decode(send(self.request("GET", "v1/storage"), Body::Empty)?)
    }

    /// Deletes finished takes' audio of `kind` (`original`), or all of it.
    /// Their text stays in history.
    pub fn purge_audio(&self, kind: Option<&str>) -> Result<(), ApiError> {
        let body = match kind {
            Some(kind) => json!({ "kind": kind }),
            None => json!({}),
        };
        send(
            self.request("POST", "v1/storage/purge-audio"),
            Body::Json(&body),
        )
        .map(drop)
    }

    /// Fifty records of shared history, older than `before` when given.
    /// `source` is `inlay` or `wispr-flow`; `None` lists both.
    pub fn history(
        &self,
        before: Option<&str>,
        source: Option<&str>,
    ) -> Result<HistoryPage, ApiError> {
        let mut request = self.request("GET", "v1/generations").query("limit", "50");
        if let Some(before) = before {
            request = request.query("before", before);
        }
        if let Some(source) = source {
            request = request.query("source", source);
        }
        decode(send(request, Body::Empty)?)
    }

    /// Removes a finished generation and its files from shared history.
    pub fn delete(&self, id: &str) -> Result<(), ApiError> {
        send(
            self.request("DELETE", &format!("v1/generations/{id}")),
            Body::Empty,
        )
        .map(drop)
    }

    /// Queues a failed or cancelled take's sealed audio to be transcribed again.
    pub fn reprocess(&self, id: &str) -> Result<(), ApiError> {
        send(
            self.request("POST", &format!("v1/generations/{id}/reprocess")),
            Body::Empty,
        )
        .map(drop)
    }

    /// Deletes a finished take's audio. Its text stays in history.
    pub fn delete_audio(&self, id: &str) -> Result<(), ApiError> {
        send(
            self.request("DELETE", &format!("v1/generations/{id}/audio")),
            Body::Empty,
        )
        .map(drop)
    }

    /// The whole record, for the History page.
    pub fn detail(&self, id: &str) -> Result<Detail, ApiError> {
        decode(send(
            self.request("GET", &format!("v1/generations/{id}")),
            Body::Empty,
        )?)
    }

    /// Downloads one of a generation's files, such as `inference.wav`.
    pub fn artifact(&self, id: &str, filename: &str) -> Result<Vec<u8>, ApiError> {
        let request = self
            .request("GET", &format!("v1/generations/{id}/artifacts/{filename}"))
            .set("Accept", "*/*");
        let mut body = Vec::new();
        send(request, Body::Empty)?
            .into_reader()
            .take(ARTIFACT_LIMIT + 1)
            .read_to_end(&mut body)
            .map_err(|_| ApiError::Disconnected)?;
        if body.len() as u64 > ARTIFACT_LIMIT {
            return Err(ApiError::Invalid("file too large".into()));
        }
        Ok(body)
    }

    pub fn generation(&self, id: &str) -> Result<Generation, ApiError> {
        decode(send(
            self.request("GET", &format!("v1/generations/{id}")),
            Body::Empty,
        )?)
    }

    pub fn cancel(&self, id: &str) -> Result<(), ApiError> {
        send(
            self.request("POST", &format!("v1/generations/{id}/cancel")),
            Body::Empty,
        )
        .map(drop)
    }

    pub fn delivery(&self, id: &str, status: &str, message: &str) -> Result<(), ApiError> {
        let body = json!({
            "status": status,
            "message": message,
            "reportedAt": humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        });
        send(
            self.request("POST", &format!("v1/generations/{id}/delivery")),
            Body::Json(&body),
        )
        .map(drop)
    }

    /// Follows NDJSON progress until the generation is terminal. A dropped
    /// stream is recovered from the stored record, then reconnected.
    pub fn events(
        &self,
        id: &str,
        mut on_update: impl FnMut(&Generation),
    ) -> Result<Generation, ApiError> {
        let mut failures = 0u32;
        loop {
            match self.read_events(id, &mut on_update) {
                Err(error) if error.is_transient() => {
                    log!("events: {id} stream interrupted ({error}); recovering")
                }
                result => return result,
            }
            match self.generation(id) {
                Ok(record) => {
                    on_update(&record);
                    if record.is_terminal() {
                        return Ok(record);
                    }
                    failures = 0;
                }
                Err(error) if error.is_transient() && failures < 4 => failures += 1,
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_secs(8.min(1 << failures)));
        }
    }

    fn read_events(
        &self,
        id: &str,
        on_update: &mut impl FnMut(&Generation),
    ) -> Result<Generation, ApiError> {
        let request = self
            .request("GET", &format!("v1/generations/{id}/events"))
            .set("Accept", "application/x-ndjson");
        let mut reader = BufReader::new(send(request, Body::Empty)?.into_reader());
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = (&mut reader)
                .take(EVENT_LINE_LIMIT + 1)
                .read_until(b'\n', &mut line)
                .map_err(|_| ApiError::Disconnected)?;
            if read == 0 {
                return Err(ApiError::Disconnected);
            }
            if line.len() as u64 > EVENT_LINE_LIMIT {
                return Err(ApiError::Invalid("event line too long".into()));
            }
            // A line cut off by a dropped connection is recovered, not rejected.
            if line.last() != Some(&b'\n') {
                return Err(ApiError::Disconnected);
            }
            if line.trim_ascii().is_empty() {
                continue;
            }
            let record: Generation = serde_json::from_slice(&line)
                .map_err(|error| ApiError::Invalid(error.to_string()))?;
            if !record.id.eq_ignore_ascii_case(id) {
                return Err(ApiError::Invalid("event for another generation".into()));
            }
            on_update(&record);
            if record.is_terminal() {
                return Ok(record);
            }
        }
    }
}

fn send(request: ureq::Request, body: Body) -> Result<ureq::Response, ApiError> {
    let result = match body {
        Body::Empty => request.call(),
        Body::Json(value) => request.send_json(value),
        Body::Bytes(bytes) => request.send_bytes(bytes),
    };
    match result {
        Ok(response) if (200..300).contains(&response.status()) => Ok(response),
        Ok(response) => Err(ApiError::Rejected {
            status: response.status(),
            code: None,
            message: format!(
                "The server redirected the request (HTTP {}). Check the server address.",
                response.status()
            ),
        }),
        Err(ureq::Error::Status(status, response)) => Err(rejected(status, response)),
        // The kind is enough for the tooltip and avoids repeating the URL.
        Err(ureq::Error::Transport(transport)) => Err(ApiError::Offline(
            transport.kind().to_string().to_lowercase(),
        )),
    }
}

fn rejected(status: u16, response: ureq::Response) -> ApiError {
    #[derive(Deserialize)]
    struct Failure {
        #[serde(default)]
        code: Option<String>,
        message: String,
    }
    let failure = response
        .into_string()
        .ok()
        .and_then(|text| serde_json::from_str::<Failure>(&text).ok());
    let code = failure.as_ref().and_then(|failure| failure.code.clone());
    let detail = failure.map(|failure| failure.message.chars().take(1000).collect::<String>());
    let message = match (status, detail) {
        (401, _) => "The server rejected this PC's token. On This PC, paste the server's admin token and Connect.".to_string(),
        // The server's own wording here is about the Mac.
        (403, _) if code.as_deref() == Some("enrollment_required") => {
            "This is the server's admin token. Inlay swaps it for this PC's own credential when it connects.".into()
        }
        (_, Some(detail)) => detail,
        (403, None) => "The server refused this request.".into(),
        (409, None) => "The recording changed before the request could finish.".into(),
        (429, None) => "The server has reached its capacity. Try again shortly.".into(),
        (503, None) => "The server is online but its models are not ready.".into(),
        (_, None) => format!("The server could not complete the request (HTTP {status})."),
    };
    ApiError::Rejected {
        status,
        code,
        message,
    }
}

fn decode<T: DeserializeOwned>(response: ureq::Response) -> Result<T, ApiError> {
    let mut body = Vec::new();
    response
        .into_reader()
        .take(RESPONSE_LIMIT + 1)
        .read_to_end(&mut body)
        .map_err(|_| ApiError::Disconnected)?;
    if body.len() as u64 > RESPONSE_LIMIT {
        return Err(ApiError::Invalid("response too large".into()));
    }
    serde_json::from_slice(&body).map_err(|error| ApiError::Invalid(error.to_string()))
}
