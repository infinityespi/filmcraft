//! # filmcraft-portal
//!
//! A client for Portals, a self-hosted review server: upload a cut, then read the comments people
//! leave on its frames. Knows nothing about projects; the engine turns comments into markers.
//!
//! Built without the `http` feature (the default, and always on the web) every call that needs
//! the network returns [`Error::Unavailable`].

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use serde::{Deserialize, Serialize};

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::{complete_version, create_portal, create_version, fetch_portal, upload_file};

/// Whether this build can reach a server.
pub const AVAILABLE: bool = cfg!(feature = "http");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this build of FilmCraft can't connect to Portals")]
    Unavailable,
    #[error("{0}")]
    Invalid(String),
    #[error("couldn't reach the Portals server: {0}")]
    Network(String),
    #[error("the Portals server refused the API key")]
    Unauthorized,
    #[error("that portal no longer exists on the server")]
    NotFound,
    #[error("the Portals server said: {0}")]
    Server(String),
    #[error("couldn't read the file: {0}")]
    File(String),
    #[error("the upload was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where the server is and the key that lets this editor use it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub server: String,
    pub api_key: String,
}

/// A portal as the server returns it to the editor.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PortalData {
    pub title: String,
    pub url: String,
    pub versions: Vec<Version>,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Version {
    pub id: String,
    pub number: i64,
    pub fps_num: i64,
    pub fps_den: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Comment {
    pub id: String,
    pub version_id: String,
    pub parent_id: Option<String>,
    pub frame: Option<i64>,
    pub frame_end: Option<i64>,
    pub author: String,
    pub body: String,
    pub resolved: bool,
    pub created_at: f64,
}

/// A new portal.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Created {
    pub token: String,
    pub url: String,
}

/// An upload the server is ready to receive.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct UploadSession {
    pub version_id: String,
    pub data_url: String,
    pub complete_url: String,
}

/// What to call a new version, and its exact frame rate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NewVersion {
    pub filename: String,
    pub size: u64,
    pub fps_num: i64,
    pub fps_den: i64,
    pub sequence_start_frame: i64,
    pub note: String,
}

/// A server address in the form the client uses: no trailing slash. API keys are never sent in
/// the clear across a network, so plain HTTP is only accepted for this machine and private LANs.
pub fn normalize_server(input: &str) -> Result<String> {
    let url = input.trim().trim_end_matches('/');
    let invalid = |m: &str| Error::Invalid(m.to_string());
    let (scheme, rest) = url.split_once("://").ok_or_else(|| invalid("The server address must start with https://"))?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || url.contains(['?', '#']) || url.chars().any(char::is_whitespace) {
        return Err(invalid("That doesn't look like a server address"));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(url.to_string()),
        "http" if is_private_host(authority) => Ok(url.to_string()),
        "http" => Err(invalid("Use https:// for a server on the internet, so the API key stays private")),
        _ => Err(invalid("The server address must start with https://")),
    }
}

fn is_private_host(authority: &str) -> bool {
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h).to_ascii_lowercase();
    if host == "localhost" || host == "[::1]" {
        return true;
    }
    // Every part must be a number: 192.168.1.1.example.com is a public name, not a LAN address.
    let Ok(octets) = host.split('.').map(str::parse::<u8>).collect::<std::result::Result<Vec<_>, _>>() else { return false };
    let [a, b, _, _] = octets[..] else { return false };
    a == 127 || a == 10 || (a == 192 && b == 168) || (a == 172 && (16..32).contains(&b))
}

#[cfg(not(feature = "http"))]
mod offline {
    use super::*;
    pub fn create_portal(_: &Connection, _: &str, _: Option<f64>) -> Result<Created> {
        Err(Error::Unavailable)
    }
    pub fn create_version(_: &Connection, _: &str, _: &NewVersion) -> Result<UploadSession> {
        Err(Error::Unavailable)
    }
    pub fn upload_file(_: &Connection, _: &UploadSession, _: &str, _: &mut dyn FnMut(u64) -> bool) -> Result<()> {
        Err(Error::Unavailable)
    }
    pub fn complete_version(_: &Connection, _: &UploadSession) -> Result<i64> {
        Err(Error::Unavailable)
    }
    pub fn fetch_portal(_: &Connection, _: &str) -> Result<PortalData> {
        Err(Error::Unavailable)
    }
}
#[cfg(not(feature = "http"))]
pub use offline::{complete_version, create_portal, create_version, fetch_portal, upload_file};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_accepted_and_tidied() {
        assert_eq!(normalize_server(" https://files.example.com/ ").unwrap(), "https://files.example.com");
    }

    #[test]
    fn plain_http_only_stays_on_private_networks() {
        for ok in ["http://127.0.0.1:8799", "http://localhost:8765", "http://192.168.1.183:8765", "http://10.0.0.2", "http://172.16.0.9"] {
            assert!(normalize_server(ok).is_ok(), "{ok}");
        }
        for bad in ["http://files.example.com", "http://8.8.8.8", "http://172.32.0.1", "http://192.168.1.1.evil.com"] {
            assert!(normalize_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hostile_addresses_are_refused() {
        for bad in ["", "files.example.com", "ftp://a.b", "https://", "https://u:p@a.b", "https://a.b?x=1", "https://a b", "javascript://x"] {
            assert!(normalize_server(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn server_comments_parse() {
        let json = r#"{"title":"T","url":"https://x/portal/t","expires_at":null,
          "versions":[{"id":"v","number":1,"fps_num":24000,"fps_den":1001,"filename":"a.mp4"}],
          "comments":[{"id":"c","version_id":"v","parent_id":null,"frame":120,"frame_end":null,
            "author":"Maya","body":"Warm","from_editor":false,"resolved":false,"created_at":1.5}]}"#;
        let p: PortalData = serde_json::from_str(json).unwrap();
        assert_eq!(p.comments[0].frame, Some(120));
        assert_eq!(p.versions[0].fps_den, 1001);
    }

    #[test]
    fn malformed_server_replies_are_errors_not_panics() {
        for bad in ["", "{}", "[]", r#"{"title":1}"#, r#"{"title":"t","url":"u","versions":[{}],"comments":[]}"#] {
            assert!(serde_json::from_str::<PortalData>(bad).is_err(), "{bad}");
        }
    }
}
