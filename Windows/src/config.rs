//! Device settings in `%APPDATA%\Inlay\config.json`. The server token lives in
//! Windows Credential Manager (see `credentials.rs`), never in this file.

use serde::{Deserialize, Serialize};
use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::{env, fs, io};
use url::{Host, Url};

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8391";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub endpoint: String,
    #[serde(rename = "deviceID")]
    pub device_id: String,
    pub device_name: String,
    /// The microphone's Windows name. `None` follows the system default input.
    #[serde(default)]
    pub microphone: Option<String>,
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default)]
    pub mute_output_while_recording: bool,
}

fn default_hotkey() -> String {
    crate::hotkey::DEFAULT_KEY.to_string()
}

/// `INLAY_CLIENT_DATA_DIR` isolates development runs, as on the Mac client.
pub fn data_dir() -> PathBuf {
    if let Some(directory) = env::var_os("INLAY_CLIENT_DATA_DIR") {
        return PathBuf::from(directory);
    }
    let base = env::var_os("APPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("Inlay")
}

impl Config {
    /// Loads saved settings. The first run creates them with a new device ID.
    /// An unreadable file is left alone so its device identity is not lost.
    pub fn load() -> Config {
        let path = data_dir().join("config.json");
        let defaults = || Config {
            endpoint: DEFAULT_ENDPOINT.into(),
            device_id: uuid::Uuid::new_v4().to_string(),
            device_name: env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into()),
            microphone: None,
            hotkey: default_hotkey(),
            mute_output_while_recording: false,
        };
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                log!(
                    "config: {} is invalid ({error}); using defaults without saving",
                    path.display()
                );
                defaults()
            }),
            Err(_) => {
                let config = defaults();
                if let Err(error) = config.save() {
                    log!("config: could not create {}: {error}", path.display());
                }
                config
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let directory = data_dir();
        fs::create_dir_all(&directory)?;
        let temporary = directory.join("config.json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(&temporary, directory.join("config.json"))
    }
}

/// Mirrors the Mac client's `ServerEndpoint`: HTTPS anywhere, plain HTTP only
/// for localhost and Tailscale addresses, and never credentials in the URL.
pub fn normalize_endpoint(value: &str) -> Result<String, String> {
    const INVALID: &str =
        "Enter an HTTP or HTTPS server address without credentials, a query, or a fragment.";
    let address = value.trim().trim_matches('/');
    let url = Url::parse(address).map_err(|_| INVALID.to_string())?;
    let secure = match url.scheme() {
        "https" => true,
        "http" => false,
        _ => return Err(INVALID.into()),
    };
    let host = url.host().ok_or(INVALID)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID.into());
    }
    if !secure && !permits_http(&host) {
        return Err(
            "Use HTTPS for this server, or HTTP with localhost or a Tailscale IP address.".into(),
        );
    }
    Ok(address.to_string())
}

fn permits_http(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => {
            let [first, second, ..] = address.octets();
            first == 127 || (first == 100 && (64..=127).contains(&second))
        }
        Host::Ipv6(address) => {
            let segments = address.segments();
            *address == Ipv6Addr::LOCALHOST || segments[..3] == [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

pub fn validate_device_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err("Use a device name between 1 and 120 characters.".into());
    }
    Ok(name.to_string())
}

/// Credential Manager holds at most 2,560 bytes per secret.
pub fn validate_token(value: &str) -> Result<String, String> {
    let token = value.trim();
    if token.len() > 2560 || token.chars().any(char::is_whitespace) {
        return Err("Use a server token without spaces, up to 2,560 bytes.".into());
    }
    Ok(token.to_string())
}
