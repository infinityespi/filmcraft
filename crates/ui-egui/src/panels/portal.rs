//! File ▸ Portal: the Connect and Upload dialogs, and copying or opening the portal link. The work
//! itself is in the engine's `portal.*` commands.

use egui::{Align2, Color32, Rect, Stroke, pos2, vec2};
use filmcraft_engine::portal::{self as engine, PortalStatus, PortalView};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::theme::Tokens;

#[derive(Clone, Debug)]
pub enum PortalDialog {
    Connect { server: String, api_key: String, then_upload: bool },
    Upload { title: String, note: String, expires_days: u32 },
}

const EXPIRY_CHOICES: [u32; 4] = [0, 7, 30, 90];

fn upload_dialog(app: &FilmcraftApp) -> PortalDialog {
    let seq = app.session.state.active_sequence;
    let title = seq.and_then(|id| app.session.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
    PortalDialog::Upload { title, note: String::new(), expires_days: 30 }
}

/// Commands that need the host: dialogs when called without parameters, and the link actions.
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let no_link = || Err(tl!("This sequence isn't on a portal yet").to_string());
    match id {
        "portal.connect" if params.get("server").is_none() => {
            // a project's portal names its own server: offer that one
            let prefill = params.get("prefill").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
            let server = prefill.or_else(|| engine::saved_server(&mut app.session)).unwrap_or_default();
            app.ui.portal_dialog = Some(PortalDialog::Connect { server, api_key: String::new(), then_upload: false });
            Some(Ok(json!({"dialog": "portalConnect"})))
        }
        "portal.upload" if params.get("title").is_none() && params.get("wait").is_none() => {
            if !app.session.is_enabled("portal.upload") {
                return None;
            }
            app.ui.portal_dialog = Some(if engine::saved_server(&mut app.session).is_some() {
                upload_dialog(app)
            } else {
                PortalDialog::Connect { server: String::new(), api_key: String::new(), then_upload: true }
            });
            Some(Ok(json!({"dialog": "portalUpload"})))
        }
        "portal.copyLink" => Some(match engine::active_url(&app.session) {
            Some(url) => {
                ctx.copy_text(url.clone());
                app.ui.status = tl!("Portal link copied").to_string();
                Ok(json!({"url": url}))
            }
            None => no_link(),
        }),
        "portal.copyCode" => Some(match engine::active_code(&app.session) {
            Some(code) => {
                ctx.copy_text(code.clone());
                app.ui.status = tl!("Review code copied").to_string();
                Ok(json!({"code": code}))
            }
            None => Err(tl!("This portal has no review code").to_string()),
        }),
        "portal.openLink" => Some(match engine::active_url(&app.session) {
            Some(url) => {
                crate::links::open(app, ctx, &url);
                Ok(json!({"url": url}))
            }
            None => no_link(),
        }),
        _ => None,
    }
}

pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.ui.portal_dialog.clone() else { return };
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    let mut push = |id: &str, r: &egui::Response, label: &str| elems.push((id.to_string(), r.rect, label.to_string()));
    let mut close = crate::widgets::escape_closes(ctx);
    let mut submit = false;
    let (key, shown) = match &dialog {
        PortalDialog::Connect { .. } => ("Connect to Portals", tl!("Connect to Portals")),
        PortalDialog::Upload { .. } => ("Upload to Portal", tl!("Upload to Portal")),
    };
    let linked = engine::active_url(&app.session).is_some();
    egui::Window::new(shown)
        .id(egui::Id::unique(key))
        .collapsible(false)
        .resizable(false)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            match &mut dialog {
                PortalDialog::Connect { server, api_key, .. } => {
                    egui::Grid::new("portal-connect").num_columns(2).show(ui, |ui| {
                        ui.label(tl!("Server:"));
                        let r = ui.add(egui::TextEdit::singleline(server).hint_text("https://files.example.com").desired_width(300.0));
                        push("portal.connect.server", &r, "Server");
                        ui.end_row();
                        ui.label(tl!("API key:"));
                        let r = ui.add(egui::TextEdit::singleline(api_key).password(true).desired_width(300.0));
                        push("portal.connect.key", &r, "API key");
                        ui.end_row();
                    });
                    ui.small(tl!("The key is saved on this computer, never in the project."));
                }
                PortalDialog::Upload { title, note, expires_days } => {
                    egui::Grid::new("portal-upload").num_columns(2).show(ui, |ui| {
                        ui.label(tl!("Title:"));
                        let r = ui.add_enabled(!linked, egui::TextEdit::singleline(title).desired_width(300.0));
                        push("portal.upload.title", &r, "Title");
                        ui.end_row();
                        ui.label(tl!("Note:"));
                        let r = ui.add(egui::TextEdit::singleline(note).hint_text(tl!("What changed? (optional)")).desired_width(300.0));
                        push("portal.upload.note", &r, "Note");
                        ui.end_row();
                        if !linked {
                            ui.label(tl!("Link expires:"));
                            let label = |d: u32| match d {
                                0 => tl!("Never").to_string(),
                                d => crate::i18n::fmt(tl!("In {days} days"), &[("days", &d.to_string())]),
                            };
                            let combo = craft_ui::ComboBox::from_id_salt("portal-expiry").selected_text(label(*expires_days)).show_ui(ui, |ui| {
                                for d in EXPIRY_CHOICES {
                                    ui.selectable_value(expires_days, d, label(d));
                                }
                            });
                            push("portal.upload.expiry", &combo.response, "Link expires");
                            ui.end_row();
                        }
                    });
                    ui.small(if linked {
                        tl!("Exports the whole sequence and adds it as a new version of this portal.")
                    } else {
                        tl!("Exports the whole sequence as H.264 and uploads it. Comments then appear as markers.")
                    });
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                push("portal.dialog.cancel", &r, "Cancel");
                close |= r.clicked();
                let ok = match &dialog {
                    PortalDialog::Connect { .. } => tl!("Connect"),
                    PortalDialog::Upload { .. } => tl!("Upload"),
                };
                let r = ui.button(ok);
                push("portal.dialog.ok", &r, "OK");
                submit = r.clicked();
            });
        });
    for (id, rect, label) in elems {
        app.auto.add(&id, rect, &label);
    }
    app.ui.portal_dialog = Some(dialog.clone());
    if submit {
        let (id, params) = match &dialog {
            PortalDialog::Connect { server, api_key, .. } => ("portal.connect", json!({"server": server, "apiKey": api_key})),
            PortalDialog::Upload { title, note, expires_days } => ("portal.upload", json!({"title": title, "note": note, "expiresDays": expires_days})),
        };
        match app.session.execute(id, params) {
            Ok(_) => {
                close = true;
                if matches!(dialog, PortalDialog::Connect { then_upload: true, .. }) {
                    app.ui.portal_dialog = Some(upload_dialog(app));
                    return;
                }
            }
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    if close {
        app.ui.portal_dialog = None;
    }
}

// ---- the Timeline's status chip ----

const CHIP_MARGIN: f32 = 8.0;
const DETAILS_WIDTH: f32 = 340.0;
const LIVE_COLOR: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const WARN_COLOR: Color32 = Color32::from_rgb(0xe3, 0xa0, 0x08);
const STOP_COLOR: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

/// A Timeline chip ready to draw: what it says and how wide it is.
pub struct Chip {
    view: PortalView,
    label: String,
    pub width: f32,
}

/// Icon and words together, so no state relies on its color alone.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    None,
    Dot(Color32),
    Warning(Color32),
    Progress,
}

fn mark(status: PortalStatus) -> Mark {
    match status {
        PortalStatus::Share => Mark::None,
        PortalStatus::Exporting | PortalStatus::Uploading => Mark::Progress,
        PortalStatus::Live => Mark::Dot(LIVE_COLOR),
        PortalStatus::Offline | PortalStatus::NeedsConnect => Mark::Warning(WARN_COLOR),
        PortalStatus::Reconnect | PortalStatus::Unavailable => Mark::Warning(STOP_COLOR),
    }
}

fn percent(progress: f32) -> String {
    format!("{}%", (progress.clamp(0.0, 1.0) * 100.0) as u32)
}

fn chip_label(v: &PortalView) -> String {
    let portal = |rest: String| if rest.is_empty() { tl!("Portal").to_string() } else { format!("{} · {rest}", tl!("Portal")) };
    match v.status {
        PortalStatus::Share => tl!("Share for review").to_string(),
        PortalStatus::Exporting => crate::i18n::fmt(tl!("Exporting {percent}"), &[("percent", &percent(v.progress))]),
        PortalStatus::Uploading => crate::i18n::fmt(tl!("Uploading {percent}"), &[("percent", &percent(v.progress))]),
        PortalStatus::Live => {
            let mut parts = Vec::new();
            if let Some(n) = v.version.filter(|n| *n > 0) {
                parts.push(format!("v{n}"));
            }
            if v.new_notes > 0 {
                parts.push(crate::i18n::fmt(tl!("{count} new"), &[("count", &v.new_notes.to_string())]));
            } else if v.open_notes > 0 {
                parts.push(crate::i18n::fmt(tl!("{count} open"), &[("count", &v.open_notes.to_string())]));
            }
            portal(parts.join(" · "))
        }
        PortalStatus::Offline => match v.synced_seconds_ago {
            Some(seconds) => crate::i18n::fmt(tl!("Offline · synced {when}"), &[("when", &ago(seconds))]),
            None => tl!("Offline").to_string(),
        },
        PortalStatus::Reconnect => tl!("Reconnect").to_string(),
        PortalStatus::NeedsConnect => tl!("Connect to sync").to_string(),
        PortalStatus::Unavailable => tl!("Portal unavailable").to_string(),
    }
}

fn ago(seconds: u64) -> String {
    let n = |value: u64, one: &str, many: &str| {
        if value == 1 { crate::i18n::t(one).to_string() } else { crate::i18n::fmt(crate::i18n::t(many), &[("count", &value.to_string())]) }
    };
    match seconds {
        0..=4 => tl!("just now").to_string(),
        5..=59 => n(seconds, "1 second ago", "{count} seconds ago"),
        60..=3599 => n(seconds / 60, "1 minute ago", "{count} minutes ago"),
        3600..=86399 => n(seconds / 3600, "1 hour ago", "{count} hours ago"),
        _ => n(seconds / 86400, "1 day ago", "{count} days ago"),
    }
}

/// A server address the way people say it.
fn host(server: &str) -> &str {
    server.split_once("://").map_or(server, |(_, rest)| rest)
}

/// The active sequence's chip, or None when there's nothing to show.
pub fn chip_for(app: &mut FilmcraftApp, ui: &egui::Ui) -> Option<Chip> {
    let view = engine::view(&mut app.session)?;
    let label = chip_label(&view);
    let text = ui.painter().layout_no_wrap(label.clone(), Tokens::ui(11.0), Color32::WHITE);
    let icon = if mark(view.status) == Mark::None { 0.0 } else { 14.0 };
    Some(Chip { width: text.size().x + icon + 16.0 + 2.0 * CHIP_MARGIN, view, label })
}

/// Draw the chip in `slot` at the right of the Timeline's tab strip, and its details box.
pub fn draw_chip(app: &mut FilmcraftApp, ui: &mut egui::Ui, slot: Rect, chip: &Chip) {
    let t = app.tokens;
    let rect = Rect::from_min_max(pos2(slot.min.x + 2.0, slot.center().y - 10.0), pos2(slot.max.x - 2.0, slot.center().y + 10.0));
    let response = ui.interact(rect, egui::Id::unique("portal-chip"), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), &chip.label));
    app.auto.add_response("portal.chip", &response, &chip.label);
    let open = app.ui.portal_details.is_some();
    let painter = ui.painter_at(slot);
    if response.hovered() || open {
        painter.rect_filled(rect, 4.0, t.hover);
    }
    let mut x = rect.min.x + CHIP_MARGIN;
    let y = rect.center().y;
    match mark(chip.view.status) {
        Mark::None => {}
        Mark::Dot(color) => {
            painter.circle_filled(pos2(x + 4.0, y), 3.5, color);
            x += 14.0;
        }
        Mark::Warning(color) => {
            let points = vec![pos2(x + 4.0, y - 5.0), pos2(x + 9.0, y + 4.0), pos2(x - 1.0, y + 4.0)];
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
            x += 14.0;
        }
        Mark::Progress => {
            let (track, fill) = (Rect::from_min_size(pos2(x, y - 1.5), vec2(9.0, 3.0)), chip.view.progress.clamp(0.0, 1.0));
            painter.rect_filled(track, 1.0, t.field_border);
            painter.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * fill, 3.0)), 1.0, t.accent);
            x += 14.0;
        }
    }
    let color = if response.hovered() || open { t.tab_text_active } else { t.tab_text };
    painter.text(pos2(x, y), Align2::LEFT_CENTER, &chip.label, Tokens::ui(11.0), color);
    let caret = rect.max.x - CHIP_MARGIN - 3.0;
    painter.add(egui::Shape::convex_polygon(vec![pos2(caret - 3.0, y - 2.0), pos2(caret + 3.0, y - 2.0), pos2(caret, y + 2.0)], color, Stroke::NONE));
    crate::dock::paint_widget_focus(&painter, &response, &t);

    let ctx = ui.ctx().clone();
    if response.clicked() {
        if chip.view.status == PortalStatus::Share {
            if let Err(e) = crate::menus::invoke(app, &ctx, "portal.upload", json!({})) {
                app.ui.status = e;
            }
        } else if open {
            app.ui.portal_details = None;
        } else {
            app.ui.portal_details = Some(DetailsState::default());
            engine::mark_seen(&mut app.session);
        }
    }
    if chip.view.status == PortalStatus::Share {
        app.ui.portal_details = None;
    }
    if app.ui.portal_details.is_some() {
        details(app, &ctx, &response, &chip.view);
    }
}

/// Stop asking before ending a link, because a later upload would start a new portal.
#[derive(Clone, Copy, Debug, Default)]
pub struct DetailsState {
    confirm_stop: bool,
}

fn status_line(v: &PortalView) -> String {
    match v.status {
        PortalStatus::Share => String::new(),
        PortalStatus::Exporting => tl!("Exporting the sequence…").to_string(),
        PortalStatus::Uploading => tl!("Uploading…").to_string(),
        PortalStatus::Live => match v.synced_seconds_ago {
            Some(seconds) => crate::i18n::fmt(tl!("Connected · synced {when}"), &[("when", &ago(seconds))]),
            None => tl!("Connected · syncing…").to_string(),
        },
        PortalStatus::Offline => tl!("Can't reach the server. Your work is safe, and syncing will keep trying.").to_string(),
        PortalStatus::Reconnect => tl!("The server refused the API key. Reconnect to keep syncing.").to_string(),
        PortalStatus::NeedsConnect => match (&v.connected_server, &v.portal_server) {
            (Some(connected), Some(portal)) => crate::i18n::fmt(
                tl!("This portal is on {portal}, but you're connected to {connected}."),
                &[("portal", host(portal)), ("connected", host(connected))],
            ),
            _ => tl!("This computer isn't connected to this portal's server.").to_string(),
        },
        PortalStatus::Unavailable => tl!("The server no longer has this portal. It may have expired or been deleted.").to_string(),
    }
}

fn details(app: &mut FilmcraftApp, ctx: &egui::Context, chip: &egui::Response, v: &PortalView) {
    let t = app.tokens;
    let mut state = app.ui.portal_details.unwrap_or_default();
    let mut elems: Vec<(&str, egui::Rect, String)> = Vec::new();
    let mut action: Option<(&str, Value)> = None;
    let mut close = crate::widgets::escape_closes(ctx);
    let linked = v.url.is_some();
    let left = (chip.rect.max.x - DETAILS_WIDTH).max(ctx.content_rect().min.x + 4.0);
    let area =
        egui::Area::new(egui::Id::unique("portal-details")).order(egui::Order::Foreground).fixed_pos(pos2(left, chip.rect.max.y + 4.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(DETAILS_WIDTH - 16.0);
                let title = v.title.clone().unwrap_or_else(|| tl!("Portal").to_string());
                ui.label(egui::RichText::new(title).font(Tokens::ui(13.0)).color(t.text).strong());
                ui.add_space(2.0);
                let line = ui.add(egui::Label::new(egui::RichText::new(status_line(v)).font(Tokens::ui(11.0)).color(t.text_dim)).wrap());
                elems.push(("portal.details.status", line.rect, status_line(v)));
                ui.add_space(6.0);
                egui::Grid::new("portal-details-grid").num_columns(2).spacing(vec2(12.0, 3.0)).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, name: &str, value: String| {
                        ui.label(egui::RichText::new(name).font(Tokens::ui(11.0)).color(t.text_faint));
                        ui.label(egui::RichText::new(value).font(Tokens::ui(11.0)).color(t.text));
                        ui.end_row();
                    };
                    if let Some(server) = v.portal_server.as_deref().or(v.connected_server.as_deref()) {
                        row(ui, tl!("Server"), host(server).to_string());
                    }
                    if let Some(code) = &v.code {
                        row(ui, tl!("Code"), code.clone());
                    }
                    if let Some(number) = v.version.filter(|n| *n > 0) {
                        let mut text = crate::i18n::fmt(tl!("Version {number}"), &[("number", &number.to_string())]);
                        if let Some(seconds) = v.uploaded_seconds_ago {
                            text = format!("{text} · {}", crate::i18n::fmt(tl!("uploaded {when}"), &[("when", &ago(seconds))]));
                        }
                        row(ui, tl!("Latest"), text);
                    }
                    if linked {
                        row(
                            ui,
                            tl!("Notes"),
                            crate::i18n::fmt(tl!("{open} open · {done} done"), &[("open", &v.open_notes.to_string()), ("done", &v.done_notes.to_string())]),
                        );
                        let opened = v
                            .opened_seconds_ago
                            .map_or_else(|| tl!("Not opened yet").to_string(), |seconds| crate::i18n::fmt(tl!("Opened {when}"), &[("when", &ago(seconds))]));
                        row(ui, tl!("Viewers"), opened);
                    }
                });
                if v.code.is_some() {
                    ui.add_space(4.0);
                    let hint =
                        egui::RichText::new(tl!("Reviewers need both. Send the code separately, like by text.")).font(Tokens::ui(11.0)).color(t.text_faint);
                    ui.add(egui::Label::new(hint).wrap());
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let mut button = |ui: &mut egui::Ui, id: &'static str, label: &str, command: &'static str, params: Value, enabled: bool| {
                        let r = ui.add_enabled(enabled, egui::Button::new(label));
                        elems.push((id, r.rect, label.to_string()));
                        if r.clicked() {
                            action = Some((command, params));
                        }
                    };
                    let busy = matches!(v.status, PortalStatus::Exporting | PortalStatus::Uploading);
                    match v.status {
                        PortalStatus::Reconnect | PortalStatus::NeedsConnect => {
                            let prefill = v.portal_server.clone().unwrap_or_default();
                            button(ui, "portal.details.connect", tl!("Connect…"), "portal.connect", json!({"prefill": prefill}), true);
                        }
                        _ => {}
                    }
                    if linked {
                        button(ui, "portal.details.copy", tl!("Copy Link"), "portal.copyLink", json!({}), true);
                        if v.code.is_some() {
                            button(ui, "portal.details.copyCode", tl!("Copy Code"), "portal.copyCode", json!({}), true);
                        }
                        button(ui, "portal.details.open", tl!("Open"), "portal.openLink", json!({}), true);
                        button(ui, "portal.details.sync", tl!("Sync Now"), "portal.sync", json!({}), v.status != PortalStatus::NeedsConnect && !busy);
                        let can_upload = matches!(v.status, PortalStatus::Live | PortalStatus::Offline);
                        button(ui, "portal.details.upload", tl!("Upload New Version"), "portal.upload", json!({}), can_upload);
                    }
                });
                if linked {
                    ui.add_space(4.0);
                    if state.confirm_stop {
                        ui.label(
                            egui::RichText::new(tl!("Stop syncing? Markers stay, but new comments won't arrive, and the next upload starts a new portal."))
                                .font(Tokens::ui(11.0))
                                .color(t.text_dim),
                        );
                        ui.horizontal(|ui| {
                            let r = ui.button(tl!("Keep Syncing"));
                            elems.push(("portal.details.stop.cancel", r.rect, "Keep Syncing".to_string()));
                            if r.clicked() {
                                state.confirm_stop = false;
                            }
                            let r = ui.button(tl!("Stop Syncing"));
                            elems.push(("portal.details.stop.confirm", r.rect, "Stop Syncing".to_string()));
                            if r.clicked() {
                                action = Some(("portal.unlink", json!({})));
                            }
                        });
                    } else {
                        let r = ui.link(egui::RichText::new(tl!("Stop syncing this sequence")).font(Tokens::ui(11.0)));
                        elems.push(("portal.details.stop", r.rect, "Stop syncing this sequence".to_string()));
                        if r.clicked() {
                            state.confirm_stop = true;
                        }
                    }
                }
            });
        });
    for (id, rect, label) in elems {
        app.auto.add(id, rect, &label);
    }
    // the chip's own click toggles, so it doesn't count as clicking away
    if area.response.clicked_elsewhere() && !chip.hovered() {
        close = true;
    }
    app.ui.portal_details = Some(state);
    if let Some((id, params)) = action {
        // Copy and Sync keep the box open for another look; the rest move on to something else.
        close |= !matches!(id, "portal.copyLink" | "portal.copyCode" | "portal.sync");
        if let Err(e) = crate::menus::invoke(app, ctx, id, params) {
            app.ui.status = e;
        }
    }
    if close {
        app.ui.portal_details = None;
    }
}
