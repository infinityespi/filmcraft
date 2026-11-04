//! The HTTP side of the client. Every call runs on a background thread, never the UI thread.

use std::fs::File;
use std::io::Read;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::json;

use super::{Comment, Connection, Created, Error, NewVersion, PortalData, Result, UploadSession};

const CHUNK_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPLY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 6;

fn agent() -> ureq::Agent {
    let provider = std::sync::Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(180)))
        .timeout_send_body(Some(Duration::from_secs(600)))
        .build()
        .into()
}

fn network(e: ureq::Error) -> Error {
    Error::Network(e.to_string())
}

/// A reply body as text, with the server's error turned into an [`Error`].
fn read(response: Result<ureq::http::Response<ureq::Body>>) -> Result<String> {
    let mut response = response?;
    let status = response.status().as_u16();
    let text = response.body_mut().with_config().limit(MAX_REPLY_BYTES).read_to_string().map_err(network)?;
    match status {
        200..=299 => Ok(text),
        401 | 403 => Err(Error::Unauthorized),
        404 | 410 => Err(Error::NotFound),
        _ => {
            let detail = serde_json::from_str::<serde_json::Value>(&text).ok();
            let detail = detail.as_ref().and_then(|v| v["detail"].as_str()).map(str::to_string);
            Err(Error::Server(detail.unwrap_or_else(|| format!("HTTP {status}"))))
        }
    }
}

fn parse<T: DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| Error::Server(format!("unexpected reply ({e})")))
}

fn post_json<T: DeserializeOwned>(c: &Connection, path: &str, body: &serde_json::Value) -> Result<T> {
    let r = agent()
        .post(format!("{}{path}", c.server))
        .header("X-Api-Key", &c.api_key)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(network);
    parse(&read(r)?)
}

pub fn create_portal(c: &Connection, title: &str, expires_at: Option<f64>) -> Result<Created> {
    post_json(c, "/api/portals", &json!({"title": title, "expires_at": expires_at}))
}

pub fn create_version(c: &Connection, token: &str, v: &NewVersion) -> Result<UploadSession> {
    let body = serde_json::to_value(v).map_err(|e| Error::Invalid(e.to_string()))?;
    post_json(c, &format!("/api/portals/{token}/versions"), &body)
}

pub fn fetch_portal(c: &Connection, token: &str) -> Result<PortalData> {
    let r = agent().get(format!("{}/api/portals/{token}", c.server)).header("X-Api-Key", &c.api_key).call().map_err(network);
    let mut portal: PortalData = parse(&read(r)?)?;
    // Oldest first, so replies always follow the comment they answer.
    portal.comments.sort_by(|a: &Comment, b: &Comment| a.created_at.total_cmp(&b.created_at));
    Ok(portal)
}

fn server_offset(c: &Connection, s: &UploadSession) -> Result<u64> {
    let r = agent().get(format!("{}{}", c.server, s.data_url)).header("X-Api-Key", &c.api_key).call().map_err(network);
    let reply: serde_json::Value = parse(&read(r)?)?;
    reply["received"].as_u64().ok_or_else(|| Error::Server("unexpected reply".into()))
}

/// Send the file, resuming from what the server holds after a failure. `keep_going` hears the
/// bytes sent so far and returns false to cancel.
pub fn upload_file(c: &Connection, s: &UploadSession, path: &str, keep_going: &mut dyn FnMut(u64) -> bool) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let file_error = |e: std::io::Error| Error::File(e.to_string());
    let mut file = File::open(path).map_err(file_error)?;
    let size = file.metadata().map_err(file_error)?.len();
    let agent = agent();
    let url = format!("{}{}", c.server, s.data_url);
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let (mut offset, mut failures) = (0u64, 0u32);
    while offset < size {
        if !keep_going(offset) {
            return Err(Error::Cancelled);
        }
        file.seek(SeekFrom::Start(offset)).map_err(file_error)?;
        let want = usize::try_from((size - offset).min(CHUNK_BYTES as u64)).unwrap_or(CHUNK_BYTES);
        let chunk = buffer.get_mut(..want).ok_or_else(|| Error::File("chunk size".into()))?;
        file.read_exact(chunk).map_err(file_error)?;
        let sent = agent
            .put(&url)
            .header("X-Api-Key", &c.api_key)
            .header("X-Upload-Offset", offset.to_string())
            .header("Content-Type", "application/octet-stream")
            .send(&*chunk)
            .map_err(network);
        match read(sent) {
            Ok(text) => {
                let reply: serde_json::Value = parse(&text)?;
                offset = reply["received"].as_u64().ok_or_else(|| Error::Server("unexpected reply".into()))?;
                failures = 0;
            }
            Err(e @ (Error::Unauthorized | Error::NotFound)) => return Err(e),
            Err(e) => {
                failures += 1;
                if failures >= MAX_ATTEMPTS {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_secs(1u64 << failures));
                if let Ok(held) = server_offset(c, s) {
                    offset = held;
                }
            }
        }
    }
    keep_going(size);
    Ok(())
}

/// Finish an upload; returns the new version's number.
pub fn complete_version(c: &Connection, s: &UploadSession) -> Result<i64> {
    let r = agent().post(format!("{}{}", c.server, s.complete_url)).header("X-Api-Key", &c.api_key).send_empty().map_err(network);
    let reply: serde_json::Value = parse(&read(r)?)?;
    reply["number"].as_i64().ok_or_else(|| Error::Server("unexpected reply".into()))
}
