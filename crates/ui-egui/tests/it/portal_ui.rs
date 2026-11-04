//! File ▸ Portal in the real UI, headless, against a small fake server on this machine: the
//! dialogs, and comments turning into markers with nobody touching the app.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{PortalLink, PortalVersionLink};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// Answers every request with the portal JSON built from the shared comments.
fn fake_server(comments: Arc<Mutex<Vec<Value>>>) -> String {
    serve(TcpListener::bind("127.0.0.1:0").unwrap(), comments)
}

fn serve_on(port: u16, comments: Arc<Mutex<Vec<Value>>>) {
    serve(TcpListener::bind(("127.0.0.1", port)).unwrap(), comments);
}

fn serve(listener: TcpListener, comments: Arc<Mutex<Vec<Value>>>) -> String {
    let address = format!("http://{}", listener.local_addr().unwrap());
    let url = address.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut seen = Vec::new();
            let mut buffer = [0u8; 2048];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&buffer[..n]),
                }
            }
            let body = json!({
                "title": "Cut", "url": format!("{url}/portal/tok"), "code": "7-crossover-clockwork",
                "versions": [{"id": "v1", "number": 1, "fps_num": 30, "fps_den": 1}],
                "comments": *comments.lock().unwrap(),
            })
            .to_string();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

fn comment(id: &str, frame: i64, author: &str, body: &str) -> Value {
    json!({"id": id, "version_id": "v1", "parent_id": null, "frame": frame, "frame_end": null,
           "author": author, "body": body, "resolved": false, "created_at": 1.0})
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(1);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn elements(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    }
    fn shot(&mut self, name: &str) {
        if let Some(dir) = &self.shots {
            let image = self.harness.render().unwrap();
            image.save(dir.join(format!("{name}.png"))).unwrap();
        }
    }
    fn markers(&mut self) -> Vec<Value> {
        self.exec("sequence.inspect", json!({}))["markers"].as_array().unwrap().clone()
    }
    /// Run frames in real time, with no commands sent, until `done` or a few seconds pass.
    fn wait_for(&mut self, mut done: impl FnMut(&Session) -> bool) -> bool {
        for _ in 0..500 {
            self.frames(1);
            if done(&self.harness.state().session) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

/// A session on a linked demo sequence, connected to `server`.
fn linked(server: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Cut", "fps": 30})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let frame_rate = s.active_sequence().unwrap().settings.frame_rate;
    let hash = filmcraft_engine::portal::timeline_hash(s.active_sequence().unwrap());
    let mut project = (*s.project).clone();
    project.portal_links.insert(
        seq,
        PortalLink {
            server: server.to_string(),
            token: "tok".to_string(),
            url: format!("{server}/portal/tok"),
            versions: vec![PortalVersionLink { id: "v1".into(), number: 1, start: Tick::ZERO, frame_rate, timeline_hash: hash }],
            notes: Default::default(),
        },
    );
    s.project = Arc::new(project);
    s.execute("portal.connect", json!({"server": server, "apiKey": "ui-test-key", "wait": true})).unwrap();
    s
}

#[test]
fn comments_become_markers_with_nobody_touching_the_app() {
    let comments = Arc::new(Mutex::new(vec![comment("a", 60, "Maya", "Warm this shot")]));
    let server = fake_server(comments.clone());
    let mut d = Driver::new(linked(&server));
    let has = |n: usize| move |s: &Session| s.active_sequence().is_some_and(|q| q.markers.len() == n);
    assert!(d.wait_for(has(1)), "the first comment never arrived");
    let m = &d.markers()[0];
    assert_eq!(m["name"], "Maya");
    assert_eq!(m["comment"], "Warm this shot");
    let steps = d.harness.state().session.history.undo.len();

    // a comment left later arrives on the next poll, again with no command
    comments.lock().unwrap().push(comment("b", 90, "Sam", "Cut tighter"));
    filmcraft_engine::portal::make_sync_due(&mut d.harness.state_mut().session);
    assert!(d.wait_for(has(2)), "the later comment never arrived");
    assert_eq!(d.harness.state().session.history.undo.len(), steps, "notes must not be undo steps");
    d.shot("portal-markers");
}

#[test]
fn a_dead_server_is_reported_once_and_the_app_keeps_working() {
    let live = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut session = linked(&live);
    // an address nothing answers on: bind a port, then free it
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    session.execute("portal.connect", json!({"server": dead, "apiKey": "k"})).unwrap();
    let mut d = Driver::new(session);
    let errors = |s: &Session| s.log.entries.iter().filter(|e| e.source == "portal").count();
    assert!(d.wait_for(|s| errors(s) >= 1), "the failure was never reported");
    for _ in 0..3 {
        filmcraft_engine::portal::make_sync_due(&mut d.harness.state_mut().session);
        d.wait_for(|s| !s.portal.wants_ticks());
    }
    assert!(errors(&d.harness.state().session) <= 2, "one failure per cause, not one per poll");
    d.exec("markers.add", json!({"time": 0, "name": "Still editing"}));
    assert_eq!(d.markers().len(), 1);
}

#[test]
fn the_connect_dialog_hides_the_key_from_automation() {
    let mut d = Driver::new(Session::default());
    d.exec("file.newSequence", json!({"name": "Cut", "fps": 30}));
    d.ok("ui.menu.invoke", json!({"command": "portal.connect", "params": {}}));
    let found = d.elements("portal.");
    for id in ["portal.connect.server", "portal.connect.key", "portal.dialog.ok", "portal.dialog.cancel"] {
        assert!(found.iter().any(|e| e == id), "{id} missing from {found:?}");
    }
    assert!(d.harness.state().ui.portal_dialog.is_some());
    let state = serde_json::to_string(&d.harness.state().ui).unwrap_or_default();
    assert!(!state.contains("portal_dialog") && !state.contains("apiKey"));
    d.shot("portal-connect");
    d.ok("ui.click", json!({"id": "portal.dialog.cancel"}));
    d.frames(3);
    assert!(d.harness.state().ui.portal_dialog.is_none());
}

#[test]
fn upload_asks_to_connect_first_then_shows_its_dialog_once_connected() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut d = Driver::new(Session::default());
    d.exec("file.newSequence", json!({"name": "My Cut", "fps": 30}));
    d.ok("ui.menu.invoke", json!({"command": "portal.upload", "params": {}}));
    assert!(d.elements("portal.").iter().any(|e| e == "portal.connect.key"));
    d.ok("ui.click", json!({"id": "portal.dialog.cancel"}));
    d.exec("portal.connect", json!({"server": server, "apiKey": "k", "wait": true}));
    d.ok("ui.menu.invoke", json!({"command": "portal.upload", "params": {}}));
    let found = d.elements("portal.");
    assert!(found.iter().any(|e| e == "portal.upload.title"), "{found:?}");
    d.shot("portal-upload");
}

#[test]
fn the_link_commands_explain_when_there_is_no_portal() {
    let mut d = Driver::new(Session::default());
    d.exec("file.newSequence", json!({"name": "Cut", "fps": 30}));
    let r = d.call("ui.menu.invoke", json!({"command": "portal.copyLink", "params": {}}));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap_or("").contains("isn't on a portal"), "{r}");
}

#[test]
fn copy_link_gives_the_portal_address() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut d = Driver::new(linked(&server));
    let r = d.ok("ui.menu.invoke", json!({"command": "portal.copyLink", "params": {}}));
    assert_eq!(r["url"], format!("{server}/portal/tok"));
}

/// A server that takes uploads: enough of the API for the whole background path.
fn upload_server() -> (String, Arc<Mutex<u64>>) {
    let received = Arc::new(Mutex::new(0u64));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let (url, total) = (address.clone(), received.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut seen = Vec::new();
            let mut buffer = [0u8; 65536];
            let head_end = loop {
                if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
                match stream.read(&mut buffer) {
                    Ok(n) if n > 0 => seen.extend_from_slice(&buffer[..n]),
                    _ => break 0,
                }
            };
            let head = String::from_utf8_lossy(&seen[..head_end]).to_lowercase();
            let length: usize = head.lines().find_map(|l| l.strip_prefix("content-length: ")?.trim().parse().ok()).unwrap_or(0);
            let mut body = seen.len().saturating_sub(head_end);
            while body < length {
                match stream.read(&mut buffer) {
                    Ok(n) if n > 0 => body += n,
                    _ => break,
                }
            }
            let line = head.lines().next().unwrap_or("").to_string();
            let reply = if line.starts_with("put ") {
                *total.lock().unwrap() += length as u64;
                json!({"received": *total.lock().unwrap()})
            } else if line.contains("/complete") {
                json!({"number": 1})
            } else if line.contains("/versions") {
                json!({"version_id": "v1", "data_url": "/up/data", "complete_url": "/up/complete"})
            } else if line.starts_with("post ") {
                json!({"token": "tok", "url": format!("{url}/portal/tok")})
            } else {
                json!({"title": "Cut", "url": format!("{url}/portal/tok"), "versions": [{"id": "v1", "number": 1, "fps_num": 30, "fps_den": 1}], "comments": []})
            }
            .to_string();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (address, received)
}

#[test]
fn the_upload_dialog_exports_and_uploads_in_the_background() {
    let (server, received) = upload_server();
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("portal.connect", json!({"server": server, "apiKey": "k", "wait": true})).unwrap();
    let mut d = Driver::new(session);
    d.ok("ui.menu.invoke", json!({"command": "portal.upload", "params": {}}));
    d.elements("portal.");
    d.ok("ui.click", json!({"id": "portal.dialog.ok"}));
    d.frames(3);
    assert!(d.harness.state().ui.portal_dialog.is_none(), "the dialog should close when the job starts");
    // long enough for a real export of the demo sequence on a busy machine
    let mut linked = false;
    for _ in 0..24000 {
        d.frames(1);
        if filmcraft_engine::portal::active_url(&d.harness.state().session).is_some() {
            linked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(linked, "the upload never finished: {}", d.exec("portal.status", json!({})));
    assert!(*received.lock().unwrap() > 1000, "no video reached the server");
    assert_eq!(d.exec("portal.status", json!({}))["versions"], 1);
    assert_eq!(d.exec("portal.status", json!({}))["uploading"], false);
}

// ---- the Timeline chip and its details box ----

impl Driver {
    fn chip(&mut self) -> Option<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": "portal.chip"}));
        v.as_array().unwrap().iter().find(|e| e["id"] == "portal.chip").map(|e| e["label"].as_str().unwrap_or("").to_string())
    }
    fn status_text(&mut self) -> String {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": "portal.details.status"}));
        v.as_array().unwrap().first().map_or(String::new(), |e| e["label"].as_str().unwrap_or("").to_string())
    }
    /// Wait for the chip to read `want`, with nothing else touching the app.
    fn wait_chip(&mut self, want: &str) {
        let mut last = None;
        for _ in 0..500 {
            last = self.chip();
            if last.as_deref().is_some_and(|l| l.contains(want)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the chip never said {want:?}; it said {last:?}");
    }
}

#[test]
fn nothing_shows_for_someone_who_never_connected() {
    let mut d = Driver::new(Session::default());
    d.exec("file.newSequence", json!({"name": "Cut", "fps": 30}));
    assert_eq!(d.chip(), None);
}

#[test]
fn connected_with_no_portal_offers_to_share() {
    let (server, _) = upload_server();
    let mut d = Driver::new(Session::default());
    d.exec("file.newSequence", json!({"name": "My Cut", "fps": 30}));
    d.exec("portal.connect", json!({"server": server, "apiKey": "k", "wait": true}));
    assert_eq!(d.chip().as_deref(), Some("Share for review"));
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    assert!(d.elements("portal.").iter().any(|e| e == "portal.upload.title"), "the chip should open the upload dialog");
    d.shot("chip-share");
}

#[test]
fn a_live_portal_shows_version_notes_and_details() {
    let comments = Arc::new(Mutex::new(vec![comment("a", 60, "Maya", "Warm this shot")]));
    let server = fake_server(comments.clone());
    let mut d = Driver::new(linked(&server));
    d.wait_chip("1 new");
    assert!(d.chip().unwrap().contains("v1"));
    d.shot("chip-live");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(4);
    let found = d.elements("portal.details");
    for id in ["portal.details.copy", "portal.details.open", "portal.details.sync", "portal.details.upload", "portal.details.stop"] {
        assert!(found.iter().any(|e| e == id), "{id} missing from {found:?}");
    }
    assert!(d.status_text().contains("Connected"), "{}", d.status_text());
    assert!(d.chip().unwrap().contains("1 open"), "opening the box clears the new count: {:?}", d.chip());
    d.shot("chip-details");
    // Escape closes it
    d.harness.key_press(egui::Key::Escape);
    d.frames(3);
    assert!(d.elements("portal.details").is_empty());
}

#[test]
fn stopping_asks_first_and_can_be_kept() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut d = Driver::new(linked(&server));
    d.wait_chip("v1");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "portal.details.stop"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "portal.details.stop.cancel"}));
    d.frames(3);
    assert!(d.chip().unwrap().contains("v1"), "keeping must leave the link alone");
    d.ok("ui.click", json!({"id": "portal.details.stop"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "portal.details.stop.confirm"}));
    d.frames(3);
    assert_eq!(d.chip().as_deref(), Some("Share for review"));
    assert!(d.elements("portal.details").is_empty());
}

#[test]
fn a_dead_server_reads_offline_and_recovers_by_itself() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let up = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut session = linked(&up);
    let seq = session.state.active_sequence.unwrap();
    let mut project = (*session.project).clone();
    project.portal_links.get_mut(&seq).unwrap().server = address.clone();
    session.project = Arc::new(project);
    session.execute("portal.connect", json!({"server": address, "apiKey": "k"})).unwrap();
    let mut d = Driver::new(session);
    d.wait_chip("Offline");
    d.shot("chip-offline");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    assert!(d.status_text().contains("keep trying"), "{}", d.status_text());
    d.harness.key_press(egui::Key::Escape);
    d.frames(3);

    // the server comes back on the same address
    let comments = Arc::new(Mutex::new(vec![comment("a", 30, "Maya", "Back")]));
    serve_on(port, comments);
    filmcraft_engine::portal::make_sync_due(&mut d.harness.state_mut().session);
    d.wait_chip("1 new");
}

#[test]
fn a_refused_key_asks_to_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        }
    });
    let up = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut session = linked(&up);
    let seq = session.state.active_sequence.unwrap();
    let mut project = (*session.project).clone();
    project.portal_links.get_mut(&seq).unwrap().server = address.clone();
    session.project = Arc::new(project);
    session.execute("portal.connect", json!({"server": address, "apiKey": "k"})).unwrap();
    let mut d = Driver::new(session);
    d.wait_chip("Reconnect");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "portal.details.connect"}));
    d.frames(3);
    assert!(d.elements("portal.").iter().any(|e| e == "portal.connect.key"));
}

#[test]
fn a_project_from_another_computer_asks_to_connect_and_names_the_server() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut session = linked(&server);
    session.execute("portal.disconnect", json!({})).unwrap();
    let mut d = Driver::new(session);
    d.wait_chip("Connect to sync");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "portal.details.connect"}));
    d.frames(3);
    match d.harness.state().ui.portal_dialog.clone() {
        Some(filmcraft_ui_egui::panels::portal::PortalDialog::Connect { server: shown, .. }) => assert_eq!(shown, server),
        other => panic!("expected the connect dialog, got {other:?}"),
    }
}

#[test]
fn the_chip_follows_the_active_sequence() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut d = Driver::new(linked(&server));
    d.wait_chip("v1");
    let linked_seq = d.harness.state().session.state.active_sequence.unwrap();
    d.exec("file.newSequence", json!({"name": "Other", "fps": 30}));
    assert_eq!(d.chip().as_deref(), Some("Share for review"));
    d.exec("sequence.open", json!({"item": linked_seq.0}));
    d.wait_chip("v1");
}

#[test]
fn the_details_box_shows_the_review_code_and_copies_it() {
    let server = fake_server(Arc::new(Mutex::new(Vec::new())));
    let mut d = Driver::new(linked(&server));
    d.wait_chip("v1");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    assert!(d.elements("portal.details").iter().any(|e| e == "portal.details.copyCode"));
    d.shot("chip-code");
    let r = d.ok("ui.menu.invoke", json!({"command": "portal.copyCode", "params": {}}));
    assert_eq!(r["code"], "7-crossover-clockwork");
    assert!(d.harness.state().ui.status.contains("copied"), "{}", d.harness.state().ui.status);
}

#[test]
fn a_server_without_codes_shows_no_code_controls() {
    let (server, _) = upload_server();
    let mut d = Driver::new(linked(&server));
    d.wait_chip("v1");
    d.ok("ui.click", json!({"id": "portal.chip"}));
    d.frames(3);
    assert!(!d.elements("portal.details").iter().any(|e| e == "portal.details.copyCode"));
    let r = d.call("ui.menu.invoke", json!({"command": "portal.copyCode", "params": {}}));
    assert_eq!(r["ok"], false, "{r}");
}
