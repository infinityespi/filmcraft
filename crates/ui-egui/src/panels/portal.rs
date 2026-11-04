//! File ▸ Portal: the Connect and Upload dialogs, and copying or opening the portal link. The work
//! itself is in the engine's `portal.*` commands.

use egui::Align2;
use filmcraft_engine::portal as engine;
use serde_json::{Value, json};

use crate::FilmcraftApp;

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
            let server = engine::saved_server(&mut app.session).unwrap_or_default();
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
