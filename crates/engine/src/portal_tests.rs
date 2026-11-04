use super::*;
use filmcraft_portal::Version;

fn version(id: &str, number: i64) -> Version {
    Version { id: id.into(), number, fps_num: 24, fps_den: 1 }
}

fn comment(id: &str, frame: i64, author: &str, body: &str) -> Comment {
    Comment {
        id: id.into(),
        version_id: "v1".into(),
        parent_id: None,
        frame: Some(frame),
        frame_end: None,
        author: author.into(),
        body: body.into(),
        resolved: false,
        created_at: 1.0,
    }
}

fn reply(id: &str, parent: &str, author: &str, body: &str) -> Comment {
    Comment { parent_id: Some(parent.into()), ..comment(id, 0, author, body) }
}

fn data(comments: Vec<Comment>) -> PortalData {
    PortalData { title: "T".into(), url: "https://x/portal/t".into(), versions: vec![version("v1", 1)], comments }
}

/// A session on an empty 24 fps sequence linked to version 1 of a portal.
fn linked() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Cut", "fps": 24})).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let frame_rate = s.active_sequence().unwrap().settings.frame_rate;
    let hash = timeline_hash(s.active_sequence().unwrap());
    outside_history(&mut s, |p| {
        p.portal_links.insert(
            seq,
            PortalLink {
                server: "https://x".into(),
                token: "t".into(),
                url: "https://x/portal/t".into(),
                versions: vec![PortalVersionLink { id: "v1".into(), number: 1, start: Tick::ZERO, frame_rate, timeline_hash: hash }],
                notes: Default::default(),
            },
        );
    });
    (s, seq)
}

fn markers(s: &Session) -> Vec<Marker> {
    s.active_sequence().unwrap().markers.clone()
}

#[test]
fn a_comment_becomes_a_marker_on_its_exact_frame() {
    let (mut s, seq) = linked();
    let r = apply_data(&mut s, seq, &data(vec![comment("a", 120, "Maya", "Warm this")]));
    assert_eq!(r, SyncSummary { added: 1, updated: 0 });
    let m = &markers(&s)[0];
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    assert_eq!(m.start, rate.tick_of(120));
    assert_eq!((m.name.as_str(), m.comment.as_str(), m.color), ("Maya", "Warm this", OPEN_COLOR));
}

#[test]
fn syncing_twice_makes_no_duplicates_and_changes_nothing() {
    let (mut s, seq) = linked();
    let d = data(vec![comment("a", 12, "Maya", "One")]);
    apply_data(&mut s, seq, &d);
    let revision = s.revision;
    assert_eq!(apply_data(&mut s, seq, &d), SyncSummary::default());
    assert_eq!(markers(&s).len(), 1);
    assert_eq!(s.revision, revision, "a sync with no news must not mark the project unsaved");
}

#[test]
fn replies_and_resolving_update_the_same_marker() {
    let (mut s, seq) = linked();
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "One")]));
    let id = markers(&s)[0].id;
    let mut done = comment("a", 12, "Maya", "One");
    done.resolved = true;
    let r = apply_data(&mut s, seq, &data(vec![done, reply("b", "a", "Sam", "Agreed")]));
    assert_eq!(r, SyncSummary { added: 0, updated: 1 });
    let m = &markers(&s)[0];
    assert_eq!((m.id, m.comment.as_str(), m.color), (id, "One\nSam: Agreed", DONE_COLOR));
    assert_eq!(markers(&s).len(), 1);
}

#[test]
fn a_deleted_marker_stays_deleted() {
    let (mut s, seq) = linked();
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "One")]));
    s.execute("markers.clearAll", json!({})).unwrap();
    assert!(markers(&s).is_empty());
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "One"), reply("b", "a", "Sam", "More")]));
    assert!(markers(&s).is_empty(), "syncing must not bring back a marker the editor removed");
}

#[test]
fn a_hand_edited_note_keeps_the_editors_text_but_still_turns_done() {
    let (mut s, seq) = linked();
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "One")]));
    let id = markers(&s)[0].id;
    s.execute("markers.edit", json!({"marker": id.0, "comment": "My own words"})).unwrap();
    let mut done = comment("a", 12, "Maya", "One");
    done.resolved = true;
    apply_data(&mut s, seq, &data(vec![done, reply("b", "a", "Sam", "More")]));
    let m = &markers(&s)[0];
    assert_eq!((m.comment.as_str(), m.color), ("My own words", DONE_COLOR));
}

#[test]
fn incoming_notes_are_not_undo_steps() {
    let (mut s, seq) = linked();
    s.execute("markers.add", json!({"time": 0, "name": "Mine"})).unwrap();
    let steps = s.history.undo.len();
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "One")]));
    assert_eq!(s.history.undo.len(), steps);
    assert_eq!(markers(&s).len(), 2);
}

#[test]
fn undo_past_a_note_brings_it_back_on_the_next_sync() {
    let (mut s, seq) = linked();
    s.execute("markers.add", json!({"time": 0, "name": "Mine"})).unwrap();
    let d = data(vec![comment("a", 12, "Maya", "One")]);
    apply_data(&mut s, seq, &d);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(markers(&s).iter().all(|m| m.name != "Mine"));
    apply_data(&mut s, seq, &d);
    assert_eq!(markers(&s).iter().filter(|m| m.name == "Maya").count(), 1);
}

#[test]
fn older_versions_and_changed_cuts_are_labelled() {
    let (mut s, seq) = linked();
    let mut d = data(vec![comment("a", 12, "Maya", "Old note")]);
    d.versions.push(version("v2", 2));
    apply_data(&mut s, seq, &d);
    assert!(markers(&s)[0].comment.starts_with("[v1] "), "{}", markers(&s)[0].comment);

    let (mut s, seq) = linked();
    outside_history(&mut s, |p| p.portal_links.get_mut(&seq).unwrap().versions[0].timeline_hash = 1);
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "Maya", "Note")]));
    assert_eq!(markers(&s)[0].comment, "[cut changed since upload] Note");
}

#[test]
fn a_label_sticks_when_replies_arrive() {
    let (mut s, seq) = linked();
    let mut d = data(vec![comment("a", 12, "Maya", "Old")]);
    d.versions.push(version("v2", 2));
    apply_data(&mut s, seq, &d);
    d.comments.push(reply("b", "a", "Sam", "Yes"));
    apply_data(&mut s, seq, &d);
    assert_eq!(markers(&s)[0].comment, "[v1] Old\nSam: Yes");
}

#[test]
fn hostile_server_data_is_harmless() {
    let (mut s, seq) = linked();
    let mut bad_rate = data(vec![comment("a", i64::MAX, "M", "huge frame"), comment("b", -5, "M", "negative")]);
    bad_rate.versions = vec![Version { id: "v1".into(), number: 1, fps_num: 0, fps_den: 0 }];
    apply_data(&mut s, seq, &bad_rate);
    let mut orphan = comment("c", 1, "M", "unknown version");
    orphan.version_id = "missing".into();
    let mut backwards = comment("d", 10, "M", "end before start");
    backwards.frame_end = Some(2);
    apply_data(&mut s, seq, &data(vec![orphan, backwards, reply("e", "nobody", "M", "orphan reply")]));
    assert!(markers(&s).iter().all(|m| m.comment != "unknown version" && m.comment != "negative" && m.duration == Tick::ZERO));
}

#[test]
fn a_range_comment_gets_a_duration() {
    let (mut s, seq) = linked();
    let mut c = comment("a", 24, "M", "Range");
    c.frame_end = Some(47);
    apply_data(&mut s, seq, &data(vec![c]));
    let rate = s.active_sequence().unwrap().settings.frame_rate;
    assert_eq!(markers(&s)[0].duration, rate.tick_of(24));
}

#[test]
fn marker_ids_never_collide_with_later_edits() {
    let (mut s, seq) = linked();
    apply_data(&mut s, seq, &data(vec![comment("a", 12, "M", "One")]));
    s.execute("markers.add", json!({"time": 0, "name": "Mine"})).unwrap();
    let ids: HashSet<u64> = markers(&s).iter().map(|m| m.id.0).collect();
    assert_eq!(ids.len(), 2);
}

#[test]
fn hostile_connect_parameters_are_refused() {
    let mut s = Session::default();
    for p in [
        json!({}),
        json!({"server": "https://x"}),
        json!({"apiKey": "k"}),
        json!({"server": "nope", "apiKey": "k"}),
        json!({"server": "http://example.com", "apiKey": "k"}),
        json!({"server": "https://x", "apiKey": "  "}),
    ] {
        assert!(s.execute("portal.connect", p).is_err());
    }
    assert!(s.portal.connection.is_none());
}

#[test]
fn upload_and_sync_explain_what_is_missing() {
    let mut s = Session::default();
    assert!(s.execute("portal.upload", json!({})).is_err());
    assert!(s.execute("portal.sync", json!({})).is_err());
    s.portal.connection = Some(Connection { server: "https://x".into(), api_key: "k".into() });
    s.portal.connection_loaded = true;
    assert!(s.execute("portal.sync", json!({})).is_err());
}

#[test]
fn the_key_is_never_part_of_the_project() {
    let (mut s, _) = linked();
    s.portal.connection = Some(Connection { server: "https://x".into(), api_key: "secret-key".into() });
    let saved = serde_json::to_string(&*s.project).unwrap();
    assert!(!saved.contains("secret-key"));
    assert!(!s.execute("portal.status", json!({})).unwrap().to_string().contains("secret-key"));
}

#[cfg(unix)]
#[test]
fn the_saved_connection_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir().join(format!("filmcraft-portal-test-{}.json", std::process::id()));
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&path, "{}").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn filenames_are_safe() {
    assert_eq!(upload_filename("Cut 1: final/final"), "Cut 1_ final_final.mp4");
    assert_eq!(upload_filename("   "), "Sequence.mp4");
}
