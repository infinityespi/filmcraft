//! **Portals** (File ▸ Portal): send the active sequence for review and keep its markers in step
//! with the comments people leave.
//!
//! `portal.upload` exports the sequence as H.264 and sends it on background jobs. While a linked
//! sequence is open, comments are fetched every [`POLL_EVERY`] and become markers.
//!
//! Incoming notes change the project outside the undo history, because they aren't the editor's
//! edits: Undo must never take back someone else's comment instead of the last cut. If Undo does
//! restore an older project, its older link is restored with it and the next sync adds the notes
//! again. A marker the editor deleted stays deleted, and one they retyped keeps their text.
//!
//! The API key lives in `portal.json` beside the preferences, readable only by this user, and
//! never in the project.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use filmcraft_portal::{Comment, Connection, NewVersion, PortalData};
use filmcraft_project::{ItemId, Label, Marker, MarkerId, MarkerKind, PortalLink, PortalNote, PortalVersionLink, Project, Sequence};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p, u64_p};
use crate::{EngineError, Job, Result, Session};

/// How often a linked sequence asks the server for new comments.
pub const POLL_EVERY: Duration = Duration::from_secs(30);
/// Marker colours: open notes stand out, finished ones read as done.
pub const OPEN_COLOR: Label = Label::Yellow;
pub const DONE_COLOR: Label = Label::Green;

type Shared<T> = Arc<Mutex<Option<std::result::Result<T, String>>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Portals work in progress for this session; nothing here is saved with the project.
#[derive(Default)]
pub struct PortalState {
    connection: Option<Connection>,
    connection_loaded: bool,
    uploads: Vec<PendingUpload>,
    sync: Option<PendingSync>,
    connect_check: Option<Shared<()>>,
    last_sync: Option<Instant>,
    last_error: Option<String>,
    /// Portals the server says are gone: don't keep asking this session.
    gone: HashSet<String>,
}

impl PortalState {
    /// Whether the UI should keep waking up for background work when nothing else is happening.
    pub fn wants_ticks(&self) -> bool {
        !self.uploads.is_empty() || self.sync.is_some() || self.connect_check.is_some()
    }
}

struct PendingUpload {
    seq: ItemId,
    export_job: u64,
    upload_job: Option<u64>,
    result: Shared<Published>,
    file: String,
    title: String,
    note: String,
    expires_at: Option<f64>,
    existing: Option<(String, String)>,
    frame_rate: FrameRate,
    start_timecode: i64,
    timeline_hash: u64,
}

struct Published {
    token: String,
    url: String,
    version_id: String,
    number: i64,
}

struct PendingSync {
    seq: ItemId,
    token: String,
    result: Shared<PortalData>,
}

/// Run a background body so a panic becomes an error, never a dead thread with a stuck result.
fn guarded<T: Send + 'static>(slot: Shared<T>, body: impl FnOnce() -> std::result::Result<T, String> + Send + 'static) {
    let run = move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| Err("internal error (see the crash log)".to_string()));
        *lock(&slot) = Some(r);
    };
    if std::thread::Builder::new().name("filmcraft-portal".into()).spawn(run).is_err() {
        log::error!("portal: couldn't start a background thread");
    }
}

// ---- the connection ----

fn connection_path(s: &Session) -> Option<PathBuf> {
    s.prefs_path.as_deref().and_then(std::path::Path::parent).map(|d| d.join("portal.json"))
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        // `mode` only applies to a new file: also tighten one that already existed.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(text.as_bytes())
}

fn connection(s: &mut Session) -> Option<Connection> {
    if !s.portal.connection_loaded {
        s.portal.connection_loaded = true;
        if s.portal.connection.is_none()
            && let Some(path) = connection_path(s)
            && let Ok(text) = std::fs::read_to_string(path)
        {
            s.portal.connection = serde_json::from_str(&text).ok();
        }
    }
    s.portal.connection.clone()
}

fn need_connection(s: &mut Session, cmd: &str) -> Result<Connection> {
    connection(s).ok_or_else(|| bad(cmd, "not connected to a Portals server (File ▸ Portal ▸ Connect…)"))
}

fn available(_: &Session) -> std::result::Result<(), String> {
    if filmcraft_portal::AVAILABLE { Ok(()) } else { Err("this build can't connect to Portals".into()) }
}

fn upload_enabled(s: &Session) -> std::result::Result<(), String> {
    available(s)?;
    s.active_sequence().map(|_| ()).ok_or_else(|| "open a sequence first".to_string())
}

fn linked_enabled(s: &Session) -> std::result::Result<(), String> {
    available(s)?;
    active_link(s).map(|_| ()).ok_or_else(|| "this sequence isn't on a portal yet".to_string())
}

fn active_link(s: &Session) -> Option<&PortalLink> {
    s.project.portal_links.get(&s.state.active_sequence?)
}

/// The portal link of the active sequence, for the host's Copy Link and Open commands.
pub fn active_url(s: &Session) -> Option<String> {
    active_link(s).map(|l| l.url.clone())
}

/// The saved server address, to prefill the Connect dialog. Never the key.
pub fn saved_server(s: &mut Session) -> Option<String> {
    connection(s).map(|c| c.server)
}

fn run_connect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "portal.connect";
    let server = str_p(p, "server").ok_or_else(|| bad(CMD, "need `server`"))?;
    let api_key = str_p(p, "apiKey").map(str::trim).filter(|k| !k.is_empty()).ok_or_else(|| bad(CMD, "need `apiKey`"))?;
    let server = filmcraft_portal::normalize_server(server).map_err(|e| bad(CMD, e.to_string()))?;
    let conn = Connection { server: server.clone(), api_key: api_key.to_string() };
    if let Some(path) = connection_path(s) {
        let text = serde_json::to_string_pretty(&conn).map_err(|e| EngineError::Other(e.to_string()))?;
        write_private(&path, &text).map_err(|e| EngineError::Other(format!("couldn't save the connection: {e}")))?;
    }
    s.portal.connection = Some(conn.clone());
    s.portal.connection_loaded = true;
    s.portal.gone.clear();
    s.portal.last_error = None;
    s.portal.last_sync = None;
    let slot: Shared<()> = Arc::default();
    let check = move || check_connection(&conn);
    if bool_p(p, "wait").unwrap_or(false) {
        check().map_err(EngineError::Other)?;
        return Ok(json!({"server": server, "connected": true}));
    }
    guarded(slot.clone(), check);
    s.portal.connect_check = Some(slot);
    Ok(json!({"server": server}))
}

/// Any answer but a refused key proves the address and key work: this token never exists.
fn check_connection(conn: &Connection) -> std::result::Result<(), String> {
    match filmcraft_portal::fetch_portal(conn, "connection-check") {
        Ok(_) | Err(filmcraft_portal::Error::NotFound) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn run_disconnect(s: &mut Session, _: &Value) -> Result<Value> {
    if let Some(path) = connection_path(s) {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(EngineError::Other(format!("couldn't remove the saved connection: {e}"))),
        }
    }
    s.portal.connection = None;
    s.portal.connection_loaded = true;
    Ok(Value::Null)
}

fn run_status(s: &mut Session, _: &Value) -> Result<Value> {
    let conn = connection(s);
    let link = active_link(s);
    Ok(json!({
        "available": filmcraft_portal::AVAILABLE,
        "server": conn.map(|c| c.server),
        "url": link.map(|l| l.url.clone()),
        "versions": link.map(|l| l.versions.len()),
        "notes": link.map(|l| l.notes.len()),
        "uploading": !s.portal.uploads.is_empty(),
        "lastError": s.portal.last_error,
    }))
}

fn run_unlink(s: &mut Session, _: &Value) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    outside_history(s, |p| {
        p.portal_links.remove(&seq);
    });
    Ok(Value::Null)
}

// ---- uploading ----

/// Fingerprint of the tracks, to spot a cut that changed since an upload. FNV-1a, not the std
/// hasher, because the value is saved in the project and must stay the same across builds.
pub fn timeline_hash(q: &Sequence) -> u64 {
    let text = serde_json::to_vec(&(&q.video_tracks, &q.audio_tracks)).unwrap_or_default();
    text.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn run_upload(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "portal.upload";
    let conn = need_connection(s, CMD)?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    if s.portal.uploads.iter().any(|u| u.seq == seq) {
        return Err(bad(CMD, "this sequence is already uploading"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (frame_rate, start_timecode, hash) = (q.settings.frame_rate, q.start_timecode, timeline_hash(q));
    let name = s.project.item(seq).map_or("Sequence", |i| i.name.as_str()).to_string();
    let title = str_p(p, "title").map(str::trim).filter(|t| !t.is_empty()).map_or(name, str::to_string);
    if title.chars().count() > 200 {
        return Err(bad(CMD, "the title is over 200 characters"));
    }
    let note = str_p(p, "note").unwrap_or("").trim().to_string();
    if note.chars().count() > 500 {
        return Err(bad(CMD, "the note is over 500 characters"));
    }
    let expires_at = u64_p(p, "expiresDays").filter(|d| *d > 0).map(|days| {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
        now + days.min(3650) as f64 * 86400.0
    });
    let existing = s.project.portal_links.get(&seq).map(|l| (l.server.clone(), l.token.clone()));
    if let Some((server, _)) = &existing
        && *server != conn.server
    {
        return Err(bad(CMD, format!("this sequence's portal is on {server}, but you're connected to {}", conn.server)));
    }
    let wait = bool_p(p, "wait").unwrap_or(false);
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let file = std::env::temp_dir().join(format!("filmcraft-portal-{}-{stamp}.mp4", std::process::id())).to_string_lossy().to_string();
    let out = s.execute("file.exportMedia", json!({"path": file, "format": "h264", "range": "entire", "wait": wait}))?;
    let export_job = out["job"].as_u64().ok_or_else(|| EngineError::Other("the export didn't start".into()))?;
    s.portal.uploads.push(PendingUpload {
        seq,
        export_job,
        upload_job: None,
        result: Arc::default(),
        file,
        title: title.clone(),
        note,
        expires_at,
        existing,
        frame_rate,
        start_timecode,
        timeline_hash: hash,
    });
    if wait {
        while s.portal.uploads.iter().any(|u| u.seq == seq) {
            advance_uploads(s);
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(e) = s.portal.last_error.clone() {
            return Err(EngineError::Other(e));
        }
        return Ok(json!({"url": s.project.portal_links.get(&seq).map(|l| l.url.clone())}));
    }
    Ok(json!({"job": export_job, "title": title}))
}

fn job_outcome(s: &Session, id: u64) -> Option<std::result::Result<(), String>> {
    let Some(job) = s.jobs.iter().find(|j| j.id == id) else {
        return Some(Err("the job disappeared".to_string()));
    };
    let result = lock(&job.result);
    result.as_ref().map(|r| r.as_ref().map(|_| ()).map_err(Clone::clone))
}

fn advance_uploads(s: &mut Session) {
    let mut i = 0;
    while i < s.portal.uploads.len() {
        let u = &s.portal.uploads[i];
        let (export_job, upload_job) = (u.export_job, u.upload_job);
        let Some(job) = upload_job else {
            match job_outcome(s, export_job) {
                None => i += 1,
                Some(Ok(())) => {
                    start_upload_job(s, i);
                    i += 1;
                }
                Some(Err(e)) => {
                    let u = s.portal.uploads.remove(i);
                    let _ = std::fs::remove_file(&u.file);
                    fail(s, format!("Portal: the export failed: {e}"));
                }
            }
            continue;
        };
        if job_outcome(s, job).is_none() {
            i += 1;
            continue;
        }
        let u = s.portal.uploads.remove(i);
        let _ = std::fs::remove_file(&u.file);
        match lock(&u.result).take() {
            Some(Ok(published)) => finish_upload(s, &u, published),
            Some(Err(e)) => fail(s, format!("Portal: {e}")),
            None => fail(s, "Portal: the upload stopped unexpectedly".to_string()),
        }
    }
}

fn fail(s: &mut Session, message: String) {
    s.portal.last_error = Some(message.clone());
    s.error_toast("portal", message);
}

fn start_upload_job(s: &mut Session, index: usize) {
    let id = s.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1;
    let job = Job { id, label: "Uploading to Portal".to_string(), progress: Default::default(), result: Default::default() };
    let (progress, job_result) = (job.progress.clone(), job.result.clone());
    s.jobs.push(job);
    let Some(conn) = s.portal.connection.clone() else {
        let message = "not connected to a Portals server".to_string();
        *lock(&job_result) = Some(Err(message.clone()));
        progress.finished.store(true, Ordering::Relaxed);
        s.portal.uploads[index].upload_job = Some(id);
        *lock(&s.portal.uploads[index].result) = Some(Err(message));
        return;
    };
    let u = &mut s.portal.uploads[index];
    u.upload_job = Some(id);
    let (file, title, note, expires_at, existing) = (u.file.clone(), u.title.clone(), u.note.clone(), u.expires_at, u.existing.clone());
    let (rate, start_timecode) = (u.frame_rate, u.start_timecode);
    let slot = u.result.clone();
    let body = {
        let progress = progress.clone();
        move || -> std::result::Result<Published, String> {
            let size = std::fs::metadata(&file).map_err(|e| e.to_string())?.len();
            progress.total.store(size.max(1), Ordering::Relaxed);
            let (token, url) = match existing {
                Some((_, token)) => {
                    let portal = filmcraft_portal::fetch_portal(&conn, &token).map_err(|e| e.to_string())?;
                    (token, portal.url)
                }
                None => {
                    let made = filmcraft_portal::create_portal(&conn, &title, expires_at).map_err(|e| e.to_string())?;
                    (made.token, made.url)
                }
            };
            let version =
                NewVersion { filename: upload_filename(&title), size, fps_num: rate.num, fps_den: rate.den, sequence_start_frame: start_timecode.max(0), note };
            let session = filmcraft_portal::create_version(&conn, &token, &version).map_err(|e| e.to_string())?;
            let mut keep_going = |sent: u64| {
                progress.done.store(sent, Ordering::Relaxed);
                !progress.cancel.load(Ordering::Relaxed)
            };
            filmcraft_portal::upload_file(&conn, &session, &file, &mut keep_going).map_err(|e| e.to_string())?;
            let number = filmcraft_portal::complete_version(&conn, &session).map_err(|e| e.to_string())?;
            Ok(Published { token, url, version_id: session.version_id, number })
        }
    };
    let finish = move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| Err("internal error (see the crash log)".to_string()));
        let outcome = match &r {
            Ok(p) => Ok(filmcraft_export::Report { path: p.url.clone(), frames: 0, seconds: 0.0, bytes: 0, render_fps: 0.0, extra_files: Vec::new() }),
            Err(e) => {
                *lock(&progress.error) = Some(e.clone());
                Err(e.clone())
            }
        };
        *lock(&slot) = Some(r);
        progress.finished.store(true, Ordering::Relaxed);
        *lock(&job_result) = Some(outcome);
    };
    if std::thread::Builder::new().name("filmcraft-portal-upload".into()).spawn(finish).is_err() {
        log::error!("portal: couldn't start the upload thread");
    }
}

fn upload_filename(title: &str) -> String {
    let safe: String = title.chars().map(|c| if c.is_alphanumeric() || " ._-".contains(c) { c } else { '_' }).take(120).collect();
    let safe = safe.trim();
    format!("{}.mp4", if safe.is_empty() { "Sequence" } else { safe })
}

fn finish_upload(s: &mut Session, u: &PendingUpload, published: Published) {
    let version =
        PortalVersionLink { id: published.version_id, number: published.number, start: Tick::ZERO, frame_rate: u.frame_rate, timeline_hash: u.timeline_hash };
    let (seq, url, token) = (u.seq, published.url, published.token);
    let server = s.portal.connection.as_ref().map(|c| c.server.clone()).unwrap_or_default();
    outside_history(s, |p| {
        let link = p.portal_links.entry(seq).or_insert_with(|| PortalLink { server, token, url: url.clone(), versions: Vec::new(), notes: Default::default() });
        link.url = url.clone();
        link.versions.push(version);
    });
    s.portal.last_error = None;
    s.portal.last_sync = None;
    s.toast(format!("Uploaded to Portal: {url}"));
}

// ---- comments to markers ----

/// Change the project without an undo step: incoming notes aren't the editor's edits.
fn outside_history(s: &mut Session, f: impl FnOnce(&mut Project)) {
    let mut p = (*s.project).clone();
    f(&mut p);
    s.project = Arc::new(p);
    s.bump_project();
    s.sync_persistence();
}

/// What a sync changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub added: usize,
    pub updated: usize,
}

fn note_text(prefix: &str, comment: &Comment, comments: &[Comment]) -> String {
    let mut text = format!("{prefix}{}", comment.body);
    for reply in comments.iter().filter(|r| r.parent_id.as_deref() == Some(comment.id.as_str())) {
        text.push_str(&format!("\n{}: {}", reply.author, reply.body));
    }
    text
}

/// Bring a sequence's markers and link up to date with what the server holds.
fn apply_comments(q: &mut Sequence, link: &mut PortalLink, data: &PortalData, next_id: &mut u64) -> SyncSummary {
    let mut summary = SyncSummary::default();
    let latest = data.versions.iter().map(|v| v.number).max().unwrap_or(0);
    let mut current_hash = None;
    let top_level = data.comments.iter().filter(|c| c.parent_id.is_none()).filter(|c| c.frame.is_some_and(|f| f >= 0));
    for comment in top_level {
        let color = if comment.resolved { DONE_COLOR } else { OPEN_COLOR };
        if let Some(note) = link.notes.get_mut(&comment.id) {
            let text = note_text(&note.prefix, comment, &data.comments);
            if text == note.text && comment.resolved == note.resolved {
                continue;
            }
            let Some(marker) = q.markers.iter_mut().find(|m| m.id == note.marker) else {
                // Deleted by the editor: remember the news, but don't bring the marker back.
                note.text = text;
                note.resolved = comment.resolved;
                continue;
            };
            if marker.comment == note.text {
                marker.comment = text.clone();
            }
            marker.color = color;
            note.text = text;
            note.resolved = comment.resolved;
            summary.updated += 1;
            continue;
        }
        let Some(version) = data.versions.iter().find(|v| v.id == comment.version_id) else { continue };
        let known = link.versions.iter().find(|v| v.id == comment.version_id);
        let start = known.map_or(Tick::ZERO, |v| v.start);
        let rate = FrameRate { num: version.fps_num, den: version.fps_den };
        let position = |frame: i64| Tick(start.0.saturating_add(rate.tick_of(frame).0));
        let frame = comment.frame.unwrap_or(0);
        let at = q.settings.frame_rate.snap_nearest(position(frame));
        let duration =
            comment.frame_end.filter(|end| *end >= frame).map_or(Tick::ZERO, |end| Tick(position(end.saturating_add(1)).0.saturating_sub(at.0).max(0)));

        let mut prefix = String::new();
        if version.number != latest {
            prefix.push_str(&format!("[v{}] ", version.number));
        }
        let hash = *current_hash.get_or_insert_with(|| timeline_hash(q));
        if known.is_none_or(|v| v.timeline_hash != hash) {
            prefix.push_str("[cut changed since upload] ");
        }
        let text = note_text(&prefix, comment, &data.comments);
        let id = MarkerId(*next_id);
        *next_id = next_id.saturating_add(1);
        q.markers.push(Marker {
            url: String::new(),
            custom_color: None,
            author: comment.author.clone(),
            id,
            start: at,
            duration,
            name: comment.author.clone(),
            comment: text.clone(),
            kind: MarkerKind::Comment,
            color,
        });
        link.notes.insert(comment.id.clone(), PortalNote { marker: id, text, prefix, resolved: comment.resolved });
        summary.added += 1;
    }
    if summary.added > 0 {
        q.markers.sort_by_key(|m| m.start);
    }
    summary
}

fn apply_data(s: &mut Session, seq: ItemId, data: &PortalData) -> SyncSummary {
    if s.project.sequence(seq).is_none() || !s.project.portal_links.contains_key(&seq) {
        return SyncSummary::default();
    }
    // Work on copies first, so a sync with no news leaves the project and its unsaved state alone.
    let (Some(mut q), Some(mut link)) = (s.project.sequence(seq).cloned(), s.project.portal_links.get(&seq).cloned()) else {
        return SyncSummary::default();
    };
    let mut next_id = s.project.next_id;
    let summary = apply_comments(&mut q, &mut link, data, &mut next_id);
    if link != s.project.portal_links[&seq] || q != *s.project.sequence(seq).unwrap_or(&q) {
        outside_history(s, |p| {
            p.next_id = next_id;
            if let Some(slot) = p.sequence_mut(seq) {
                *slot = q;
            }
            p.portal_links.insert(seq, link);
        });
    }
    if summary.added > 0 {
        s.toast(match summary.added {
            1 => "Portal: 1 new note".to_string(),
            n => format!("Portal: {n} new notes"),
        });
    }
    summary
}

fn start_sync(s: &mut Session, conn: Connection, seq: ItemId, token: String) {
    let result: Shared<PortalData> = Arc::default();
    let t = token.clone();
    guarded(result.clone(), move || filmcraft_portal::fetch_portal(&conn, &t).map_err(|e| e.to_string()));
    s.portal.sync = Some(PendingSync { seq, token, result });
    s.portal.last_sync = Some(Instant::now());
}

fn run_sync(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "portal.sync";
    let conn = need_connection(s, CMD)?;
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let token = s.project.portal_links.get(&seq).map(|l| l.token.clone()).ok_or_else(|| bad(CMD, "this sequence isn't on a portal yet"))?;
    s.portal.gone.remove(&token);
    if bool_p(p, "wait").unwrap_or(false) {
        let data = filmcraft_portal::fetch_portal(&conn, &token).map_err(|e| EngineError::Other(e.to_string()))?;
        s.portal.last_sync = Some(Instant::now());
        s.portal.last_error = None;
        let r = apply_data(s, seq, &data);
        return Ok(json!({"added": r.added, "updated": r.updated, "notes": data.comments.len()}));
    }
    if s.portal.sync.is_none() {
        start_sync(s, conn, seq, token);
    }
    Ok(Value::Null)
}

fn finish_sync(s: &mut Session) {
    let Some(sync) = &s.portal.sync else { return };
    let Some(result) = lock(&sync.result).take() else { return };
    let (seq, token) = (sync.seq, sync.token.clone());
    s.portal.sync = None;
    match result {
        Ok(data) => {
            s.portal.last_error = None;
            apply_data(s, seq, &data);
        }
        Err(e) => {
            if e == filmcraft_portal::Error::NotFound.to_string() {
                s.portal.gone.insert(token);
            }
            // Say so once: a dropped connection shouldn't pop a message every 30 seconds.
            if s.portal.last_error.as_deref() != Some(e.as_str()) {
                s.error_toast("portal", format!("Portal: {e}"));
                s.portal.last_error = Some(e);
            }
        }
    }
}

/// Apply finished background work and start the next sync when one is due. Called once per UI
/// frame from [`Session::poll_persistence`].
pub fn poll(s: &mut Session) {
    let checked = s.portal.connect_check.as_ref().and_then(|check| lock(check).take());
    if let Some(result) = checked {
        s.portal.connect_check = None;
        match result {
            Ok(()) => s.toast("Connected to Portals"),
            Err(e) => fail(s, format!("Portal: {e}")),
        }
    }
    advance_uploads(s);
    finish_sync(s);
    if !filmcraft_portal::AVAILABLE || s.portal.sync.is_some() {
        return;
    }
    let Some(seq) = s.state.active_sequence else { return };
    let Some(token) = s.project.portal_links.get(&seq).map(|l| l.token.clone()) else { return };
    if s.portal.gone.contains(&token) || s.portal.last_sync.is_some_and(|t| t.elapsed() < POLL_EVERY) {
        return;
    }
    if let Some(conn) = connection(s) {
        start_sync(s, conn, seq, token);
    }
}

/// Make the next [`poll`] fetch comments now instead of at the end of the wait.
pub fn make_sync_due(s: &mut Session) {
    s.portal.last_sync = None;
}

/// Whether the host should keep waking up so [`poll`] runs: a linked sequence is open, or
/// background work is running.
pub fn needs_polling(s: &Session) -> bool {
    filmcraft_portal::AVAILABLE && (s.portal.wants_ticks() || active_link(s).is_some())
}

type Enabled = fn(&Session) -> std::result::Result<(), String>;
type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    // not journalled: parameters can hold the API key, and these talk to a server
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    const MENU: &[&str] = &["File", "Portal"];
    vec![
        spec("portal.upload", "Upload Sequence to Portal…", MENU, r#"{"title":str?,"note":str?,"expiresDays":u64?,"wait":bool?}"#, upload_enabled, run_upload),
        spec("portal.sync", "Sync Comments Now", MENU, r#"{"wait":bool?}"#, linked_enabled, run_sync),
        spec("portal.connect", "Connect to Server…", MENU, r#"{"server":str,"apiKey":str,"wait":bool?}"#, available, run_connect),
        spec("portal.disconnect", "Disconnect from Server", MENU, "{}", available, run_disconnect),
        spec("portal.unlink", "Stop Syncing this Sequence", MENU, "{}", linked_enabled, run_unlink),
        spec("portal.status", "Portal Status", &[], "{}", always, run_status),
    ]
}

#[cfg(test)]
#[path = "portal_tests.rs"]
mod tests;
