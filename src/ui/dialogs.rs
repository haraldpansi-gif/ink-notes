//! Modale Dialoge, Dateiauswahl, Update-Center, Toast-Meldungen und Notfall-Wiederherstellung.

use crate::document::Notebook;
use crate::file_picker::PickerKind;
use crate::storage::NotebookSummary;
use crate::t;

use super::widgets::{
    dialog_header, dialog_width, dialog_window, layout_picker, touch_friendly_spacing,
};
use super::{MoveSubject, UiAction, UiState};

pub(super) fn render_dialogs(
    ctx: &egui::Context,
    state: &mut UiState,
    notebook: &Notebook,
    updater: &crate::updater::UpdaterController,
    actions: &mut Vec<UiAction>,
) {
    // 3a. Volltext-Suche (Strg+F)
    if state.is_search_open {
        let width = dialog_width(ctx, state, 440.0);
        let search_win = dialog_window(t!("Suchen"), state, false).show(ctx, |ui| {
            ui.set_width(width);
            touch_friendly_spacing(ui);
            if dialog_header(ui, t!("🔍 Suchen"), true) {
                state.is_search_open = false;
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.search_query)
                    .hint_text(t!("Titel, Text, LaTeX oder PDF suchen …"))
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 8.0)),
            );
            if state.search_focus {
                edit.request_focus();
                state.search_focus = false;
            }
            if edit.changed() {
                actions.push(UiAction::RunSearch);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                use crate::search::SearchScope;
                for (scope, label) in [
                    (SearchScope::CurrentNotebook, t!("Dieses Notizbuch")),
                    (SearchScope::AllNotebooks, t!("Alle Notizbücher")),
                ] {
                    if ui
                        .selectable_label(state.search_scope == scope, label)
                        .clicked()
                        && state.search_scope != scope
                    {
                        state.search_scope = scope;
                        actions.push(UiAction::RunSearch);
                    }
                }
            });
            ui.add_space(6.0);
            if state.search_query.trim().is_empty() {
                ui.label(
                    egui::RichText::new(t!("Suchbegriff eingeben (Strg+F)."))
                        .color(egui::Color32::from_gray(150)),
                );
            } else if state.search_hits.is_empty() {
                ui.label(
                    egui::RichText::new(t!("Keine Treffer gefunden."))
                        .color(egui::Color32::from_gray(150)),
                );
            } else {
                let hits_snapshot = state.search_hits.clone();
                let show_nb = state.search_scope == crate::search::SearchScope::AllNotebooks;
                egui::ScrollArea::vertical()
                    .id_salt("search_hits_scroll")
                    .max_height(320.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for hit in &hits_snapshot {
                            let header = if show_nb {
                                format!("{} · S. {}", hit.notebook_title, hit.page_index + 1)
                            } else {
                                t!("Seite {number}", number = hit.page_index + 1)
                            };
                            let text = egui::RichText::new(format!("{header}\n{}", hit.snippet))
                                .size(12.5);
                            if ui
                                .add_sized([ui.available_width(), 42.0], egui::Button::new(text))
                                .clicked()
                            {
                                actions.push(UiAction::JumpToSearchHit {
                                    notebook_id: hit.notebook_id.clone(),
                                    page_index: hit.page_index,
                                });
                                state.is_search_open = false;
                            }
                        }
                    });
            }
        });
        state.search_modal_rect = search_win.map(|r| r.response.rect);
    } else {
        state.search_modal_rect = None;
    }

    // 3b. Dokumenten-Manager (Notizbücher wechseln, anlegen, umbenennen, löschen)
    if state.is_document_menu_open {
        let width = dialog_width(ctx, state, 420.0);
        let doc_win = dialog_window(t!("Dokumenten-Manager"), state, false)
            .show(ctx, |ui| {
                ui.set_width(width);
                touch_friendly_spacing(ui);
                if dialog_header(ui, t!("📚 Notizbücher"), true) {
                    state.is_document_menu_open = false;
                }

                // Gezeigter Ordner (gibt es ihn nicht mehr, die oberste Ebene)
                if !state.notebook_folder.is_empty()
                    && !state.library.folders.contains(&state.notebook_folder)
                {
                    state.notebook_folder.clear();
                }
                let folder = state.notebook_folder.clone();
                folder_breadcrumbs(ui, state, &folder);
                ui.add_space(6.0);

                // Neues Notizbuch oder neuer Ordner hier (narrow screens: buttons below)
                ui.horizontal_wrapped(|ui| {
                    let text_edit = ui.add(
                        egui::TextEdit::singleline(&mut state.new_notebook_title_input)
                            .hint_text(t!("Name …"))
                            .margin(egui::vec2(8.0, 8.0))
                            .desired_width(190.0_f32.min(ui.available_width())),
                    );
                    let create_btn = ui
                        .button(egui::RichText::new(t!("➕ Notizbuch")).strong())
                        .on_hover_text(t!("Neues Notizbuch in diesem Ordner anlegen und öffnen"));
                    let folder_btn = ui
                        .button(egui::RichText::new(t!("📁 Ordner")).strong())
                        .on_hover_text(t!("Neuen Ordner hier anlegen (z. B. Semester oder Fach)"));
                    let title = state.new_notebook_title_input.trim().to_string();
                    if !title.is_empty() {
                        if create_btn.clicked()
                            || (text_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        {
                            actions.push(UiAction::CreateNotebook {
                                title,
                                folder: folder.clone(),
                                layout: state.new_notebook_layout,
                            });
                            state.new_notebook_title_input.clear();
                        } else if folder_btn.clicked() {
                            actions.push(UiAction::CreateFolder {
                                parent: folder.clone(),
                                name: title,
                            });
                            state.new_notebook_title_input.clear();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(t!("Seiten:"));
                    layout_picker(ui, &mut state.new_notebook_layout, true, false);
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                // Zielordner für „Verschieben“ wählen
                if let Some(moving) = state.moving.clone() {
                    move_target_list(ui, state, &moving, actions);
                    return;
                }

                let summaries: Vec<NotebookSummary> = state
                    .available_notebooks
                    .iter()
                    .filter(|s| state.library.folder_of(&s.id) == folder)
                    .cloned()
                    .collect();
                let subfolders: Vec<String> = state
                    .library
                    .subfolders(&folder)
                    .into_iter()
                    .map(str::to_string)
                    .collect();

                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    for path in &subfolders {
                        ui.push_id(path, |ui| {
                            ui.horizontal(|ui| folder_row(ui, state, path, actions))
                        });
                        ui.add_space(4.0);
                    }
                    if subfolders.is_empty() && summaries.is_empty() {
                        ui.label(
                            egui::RichText::new(t!("Dieser Ordner ist leer."))
                                .color(egui::Color32::from_gray(150)),
                        );
                    }
                    for summary in &summaries {
                        let is_current = summary.id == state.current_notebook_id;
                        ui.push_id(&summary.id, |ui| ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);

                            if state.renaming_notebook_id.as_deref() == Some(&summary.id) {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut state.renaming_title_input)
                                        .margin(egui::vec2(8.0, 8.0))
                                        .desired_width(260.0_f32.min(ui.available_width() - 80.0)),
                                );
                                if ui.button("✔").clicked()
                                    || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                {
                                    let new_t = state.renaming_title_input.trim();
                                    if !new_t.is_empty() {
                                        actions.push(UiAction::RenameNotebook {
                                            id: summary.id.clone(),
                                            new_title: new_t.to_string(),
                                        });
                                    }
                                    state.renaming_notebook_id = None;
                                }
                                if ui.button("🗙").clicked() {
                                    state.renaming_notebook_id = None;
                                }
                            } else {
                                let (bg, border) = if is_current {
                                    (
                                        egui::Color32::from_rgba_premultiplied(45, 80, 145, 200),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(110, 165, 255, 180)),
                                    )
                                } else {
                                    (
                                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8),
                                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
                                    )
                                };

                                // Nur der Titelbereich öffnet das Notizbuch: Eine Klickfläche über der
                                // ganzen Zeile würde (als zuletzt angelegtes Element) die Klicks auf
                                // Umbenennen und Löschen abfangen
                                let mut open_clicked = false;
                                egui::Frame::NONE
                                    .fill(bg)
                                    .stroke(border)
                                    .corner_radius(egui::CornerRadius::same(8))
                                    .inner_margin(egui::Margin::symmetric(10, 8))
                                    .show(ui, |ui| {
                                        // Erst die Knöpfe (rechts), der Titel füllt den Rest: Die ganze
                                        // Titelfläche öffnet das Notizbuch, ohne die Knöpfe zu überdecken
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if summary.status == crate::storage::NotebookStatus::Damaged {
                                                    if ui
                                                        .button(egui::RichText::new(t!("🔄 Backup")).size(13.0).color(egui::Color32::from_rgb(100, 200, 255)))
                                                        .on_hover_text(t!("Aus manifest.json.bak wiederherstellen"))
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::RestoreNotebookBackup(summary.id.clone()));
                                                    }
                                                    if ui
                                                        .button(egui::RichText::new(t!("📁 Ordner")).size(13.0))
                                                        .on_hover_text(t!("Pfad in Statusleiste anzeigen"))
                                                        .clicked()
                                                    {
                                                        actions.push(UiAction::OpenNotebookFolder(summary.id.clone()));
                                                    }
                                                } else {
                                                    if state.available_notebooks.len() > 1
                                                        && ui
                                                            .button(egui::RichText::new("🗑").size(16.0).color(egui::Color32::from_rgb(255, 120, 120)))
                                                            .on_hover_text(t!("Notizbuch in Papierkorb verschieben"))
                                                            .clicked()
                                                    {
                                                        actions.push(UiAction::DeleteNotebook(summary.id.clone()));
                                                    }

                                                    if ui
                                                        .button(egui::RichText::new("📂").size(16.0))
                                                        .on_hover_text(t!("In einen anderen Ordner verschieben"))
                                                        .clicked()
                                                    {
                                                        state.moving = Some(MoveSubject::Notebook(summary.id.clone()));
                                                    }

                                                    if ui.button(egui::RichText::new("✏").size(16.0))
                                                        .on_hover_text(t!("Titel umbenennen"))
                                                        .clicked()
                                                    {
                                                        state.renaming_notebook_id = Some(summary.id.clone());
                                                        state.renaming_title_input = summary.title.clone();
                                                    }
                                                }

                                            let title_area = ui
                                                .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                                    ui.vertical(|ui| {
                                                        ui.set_min_width(ui.available_width());
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&summary.title)
                                                            .strong()
                                                            .size(13.0)
                                                            .color(egui::Color32::WHITE),
                                                    );
                                                    if summary.status == crate::storage::NotebookStatus::Damaged {
                                                        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), t!("⚠ Beschädigt"));
                                                    } else if is_current {
                                                        ui.colored_label(egui::Color32::from_rgb(46, 204, 113), t!("⚫ Aktiv"));
                                                    }
                                                });
                                                let subtext = if let Some(ref err) = summary.error {
                                                    t!("Fehler: {err}", err = err)
                                                } else {
                                                    t!("{count} Seite(n)", count = summary.page_count)
                                                };
                                                ui.label(
                                                    egui::RichText::new(subtext)
                                                        .size(11.0)
                                                        .color(egui::Color32::from_gray(160)),
                                                );
                                                    })
                                                })
                                                .inner;
                                            open_clicked = title_area
                                                .response
                                                .interact(egui::Sense::click())
                                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                .clicked();
                                        });
                                    });

                                if summary.status != crate::storage::NotebookStatus::Damaged
                                    && open_clicked
                                    && !is_current
                                {
                                    actions.push(UiAction::OpenNotebook(summary.id.clone()));
                                    state.is_document_menu_open = false;
                                }
                            }
                        }));
                        ui.add_space(4.0);
                    }
                });

                if let Some((_, ref title)) = state.last_deleted_notebook {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(t!("'{title}' gelöscht", title = title))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(255, 180, 100)),
                        );
                        if ui
                            .button(egui::RichText::new(t!("↩ Rückgängig")).size(13.0).color(egui::Color32::from_rgb(100, 200, 255)))
                            .clicked()
                        {
                            actions.push(UiAction::UndoDeleteNotebook);
                        }
                    });
                }
            });
        state.document_modal_rect = doc_win.map(|w| w.response.rect);
    } else {
        state.document_modal_rect = None;
    }

    // 3c. Dateiauswahl „Bild einfügen“
    if state.file_picker.is_some() {
        file_picker_dialog(ctx, state, actions);
    }

    // 3c. Release- & Update-Center (Gitea Releases)
    if state.is_update_dialog_open {
        let width = dialog_width(ctx, state, 460.0);
        let win = dialog_window(t!("Release- & Update-Center"), state, false)
            .show(ctx, |ui| {
                ui.set_width(width);
                touch_friendly_spacing(ui);
                if dialog_header(ui, t!("🚀 Updates & Versionen"), true) {
                    state.is_update_dialog_open = false;
                }

                // Info über lokale Installation
                let current_ver = env!("CARGO_PKG_VERSION");
                let runtime_name = updater.runtime().display_name();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t!("Aktuelle Version:")).color(egui::Color32::from_gray(180)));
                    ui.label(egui::RichText::new(format!("v{}", current_ver)).strong().color(egui::Color32::WHITE));
                    ui.label(egui::RichText::new(format!("({})", runtime_name)).color(egui::Color32::from_gray(150)));
                });

                if !updater.runtime().self_update_allowed() {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(t!(
                            "Diese Version wird über die Software-Verwaltung aktualisiert \
                             (z. B. GNOME Software oder „flatpak update“)."
                        ))
                        .color(egui::Color32::from_gray(210)),
                    );
                    return;
                }

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(t!("Server:")).color(egui::Color32::from_gray(180)));
                    ui.label(egui::RichText::new(format!("{}/{}/{}", updater.config().server_url, updater.config().repo_owner, updater.config().repo_name)).color(egui::Color32::from_gray(210)));
                });

                ui.add_space(10.0);

                // Update-Prüfung auslösen
                let is_busy = matches!(updater.status(), crate::updater::UpdaterStatus::Checking | crate::updater::UpdaterStatus::Downloading { .. });
                ui.horizontal(|ui| {
                    if ui.add_enabled(!is_busy, egui::Button::new(egui::RichText::new(t!("🔄 Nach Updates suchen")).strong())).clicked() {
                        actions.push(UiAction::CheckForUpdates);
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                // Statusbereich
                match updater.status() {
                    crate::updater::UpdaterStatus::Idle => {
                        ui.label(
                            egui::RichText::new(t!("Klicke auf 'Nach Updates suchen', um die neuesten Releases vom Gitea-Server abzufragen."))
                                .color(egui::Color32::from_gray(170)),
                        );
                    }
                    crate::updater::UpdaterStatus::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(t!("Prüfe Gitea-Releases...")).color(egui::Color32::WHITE));
                        });
                    }
                    crate::updater::UpdaterStatus::UpToDate { current_version } => {
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✔");
                            ui.label(
                                egui::RichText::new(t!("ink-notes ist auf dem neuesten Stand (v{version}).", version = current_version))
                                    .color(egui::Color32::from_rgb(46, 204, 113))
                                    .strong(),
                            );
                        });
                    }
                    crate::updater::UpdaterStatus::Error(err) => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(231, 76, 60), "⚠");
                                ui.label(
                                    egui::RichText::new(t!("Abfrage fehlgeschlagen:"))
                                        .color(egui::Color32::from_rgb(231, 76, 60))
                                        .strong(),
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(err)
                                    .size(11.5)
                                    .color(egui::Color32::from_gray(190)),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(t!("Hinweis: Falls das Repository privat ist, unten ein Personal Access Token eintragen."))
                                    .size(11.0)
                                    .italics()
                                    .color(egui::Color32::from_gray(140)),
                            );
                        });
                    }
                    crate::updater::UpdaterStatus::UpdateAvailable(release) => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "⚫");
                                ui.label(
                                    egui::RichText::new(t!("Neue Version verfügbar: {version}", version = release.tag_name))
                                        .strong()
                                        .size(14.0)
                                        .color(egui::Color32::WHITE),
                                );
                            });

                            if !release.name.is_empty() && release.name != release.tag_name {
                                ui.label(egui::RichText::new(&release.name).italics().color(egui::Color32::from_gray(180)));
                            }

                            if !release.body.is_empty() {
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(t!("Änderungen:")).strong().size(11.5).color(egui::Color32::from_gray(170)));
                                egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                                    ui.label(egui::RichText::new(&release.body).size(11.5).color(egui::Color32::from_gray(210)));
                                });
                            }

                            ui.add_space(10.0);
                            ui.label(egui::RichText::new(t!("VERFÜGBARE ARTEFAKTE:")).size(10.5).strong().color(egui::Color32::from_gray(150)));
                            ui.add_space(4.0);

                            for asset in &release.assets {
                                let is_deb = asset.name.ends_with(".deb");
                                let is_appimage = asset.name.ends_with(".AppImage");
                                let is_recommended = match updater.runtime() {
                                    crate::updater::RuntimeEnvironment::AppImage { .. } => is_appimage,
                                    crate::updater::RuntimeEnvironment::DebPackage { .. } => is_deb,
                                    crate::updater::RuntimeEnvironment::WindowsExe { .. } => asset.name.ends_with(".exe"),
                                    _ => false,
                                };

                                let size_mb = (asset.size as f64) / (1024.0 * 1024.0);
                                let btn_text = if size_mb > 0.05 {
                                    format!("📥 {} ({:.1} MB)", asset.name, size_mb)
                                } else {
                                    format!("📥 {}", asset.name)
                                };

                                ui.horizontal(|ui| {
                                    let btn = if is_recommended {
                                        ui.button(egui::RichText::new(format!("⭐ {}", btn_text)).strong().color(egui::Color32::from_rgb(110, 195, 255)))
                                    } else {
                                        ui.button(egui::RichText::new(&btn_text))
                                    };

                                    if btn.clicked() {
                                        actions.push(UiAction::DownloadUpdateAsset(asset.clone()));
                                    }

                                    if is_recommended {
                                        ui.label(egui::RichText::new(t!("(Empfohlen für dieses System)")).size(10.5).color(egui::Color32::from_rgb(46, 204, 113)));
                                    }
                                });
                            }

                            ui.add_space(8.0);
                            if !release.html_url.is_empty()
                                && ui.button(egui::RichText::new(t!("🌐 Release auf Gitea öffnen")).size(12.0)).clicked()
                            {
                                actions.push(UiAction::OpenUrlInBrowser(release.html_url.clone()));
                            }
                        });
                    }
                    crate::updater::UpdaterStatus::Downloading { asset_name, percent } => {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(t!("Lade '{name}' herunter...", name = asset_name)).strong());
                            ui.add_space(6.0);
                            ui.add(egui::ProgressBar::new(*percent).show_percentage());
                        });
                    }
                    crate::updater::UpdaterStatus::DownloadFinished { file_path, asset_name } => {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(46, 204, 113), "✔");
                                ui.label(
                                    egui::RichText::new(t!("'{name}' erfolgreich heruntergeladen!", name = asset_name))
                                        .strong()
                                        .color(egui::Color32::from_rgb(46, 204, 113)),
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(t!("Gespeichert in: {path}", path = file_path.display()))
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(170)),
                            );
                            ui.add_space(8.0);

                            let action_label = match updater.runtime() {
                                crate::updater::RuntimeEnvironment::AppImage { .. } => t!("🚀 AppImage aktualisieren & ausführen"),
                                crate::updater::RuntimeEnvironment::DebPackage { .. } => t!("📦 Paket mit Software-Center öffnen / installieren"),
                                crate::updater::RuntimeEnvironment::PortableOrDev { .. }
                                | crate::updater::RuntimeEnvironment::Flatpak => t!("📂 Im Dateimanager anzeigen"),
                                crate::updater::RuntimeEnvironment::WindowsExe { .. } => t!("🔄 Update installieren"),
                            };

                            ui.horizontal(|ui| {
                                if ui.button(egui::RichText::new(action_label).strong()).clicked() {
                                    actions.push(UiAction::InstallOrOpenUpdate(file_path.clone()));
                                }
                                if ui.button(t!("📂 Ordner öffnen")).clicked() {
                                    if let Some(parent) = file_path.parent() {
                                        actions.push(UiAction::OpenUrlInBrowser(parent.to_string_lossy().to_string()));
                                    }
                                }
                            });
                        });
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);

                // Erweiterte Konfiguration (Collapsible)
                egui::CollapsingHeader::new(egui::RichText::new(t!("⚙ Erweiterte Server-Einstellungen")).size(11.5).color(egui::Color32::from_gray(160)))
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(t!("Gitea Server-URL:")).size(11.0));
                        ui.add(egui::TextEdit::singleline(&mut state.updater_server_url_input).desired_width(380.0_f32.min(ui.available_width())));

                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(t!("Personal Access Token (optional für private Repos):")).size(11.0));
                        ui.add(egui::TextEdit::singleline(&mut state.updater_token_input).password(true).desired_width(380.0_f32.min(ui.available_width())));

                        ui.add_space(6.0);
                        if ui.button(egui::RichText::new(t!("💾 Einstellungen speichern")).size(11.5)).clicked() {
                            let token = if state.updater_token_input.trim().is_empty() {
                                None
                            } else {
                                Some(state.updater_token_input.trim().to_string())
                            };
                            actions.push(UiAction::SaveUpdaterConfig {
                                server_url: state.updater_server_url_input.trim().to_string(),
                                auth_token: token,
                            });
                        }
                    });
            });
        state.update_modal_rect = win.map(|w| w.response.rect);
    } else {
        state.update_modal_rect = None;
    }

    // 4. Feedback-Toast für PDF-Export oder wichtige Systemmeldungen
    if let Some((msg, timestamp)) = &state.export_message {
        let elapsed = timestamp.elapsed().as_secs_f32();
        if elapsed < 4.0 {
            let alpha = if elapsed > 3.0 {
                ((4.0 - elapsed) * 255.0) as u8
            } else {
                255
            };

            egui::Area::new(egui::Id::unique("export_toast_banner"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -35.0))
                .order(egui::Order::Tooltip)
                .show(ctx, |ui| {
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgba_unmultiplied(
                            20,
                            25,
                            30,
                            alpha.min(235),
                        ))
                        .corner_radius(egui::CornerRadius::same(10))
                        .stroke(egui::Stroke::new(
                            1.0_f32,
                            egui::Color32::from_rgba_unmultiplied(46, 204, 113, alpha),
                        ))
                        .inner_margin(egui::Margin::symmetric(20, 10))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(alpha.min(130)),
                        })
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(format!("✔ {}", msg))
                                    .color(egui::Color32::from_rgba_unmultiplied(
                                        220, 255, 230, alpha,
                                    ))
                                    .size(13.0)
                                    .strong(),
                            );
                        });
                });
        } else {
            state.export_message = None;
        }
    }

    // 5. Nicht-wegklickbare Warnung bei Ladefehlern / Quarantäne (Block 1)
    if let Some(warning) = notebook.load_warnings.get(&notebook.current_page().id) {
        let width = dialog_width(ctx, state, 420.0);
        dialog_window(t!("⚠ Datenintegritäts-Warnung"), state, true)
            .show(ctx, |ui| {
                ui.set_width(width);
                touch_friendly_spacing(ui);
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠");
                    ui.label(
                        egui::RichText::new(t!(
                            "Seite {id} konnte nicht fehlerfrei geladen werden!",
                            id = warning.page_id
                        ))
                        .strong()
                        .size(15.0)
                        .color(egui::Color32::WHITE),
                    );
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(t!("Fehlermeldung: {message}", message = warning.message))
                        .size(12.0)
                        .color(egui::Color32::from_gray(200)),
                );
                ui.add_space(8.0);
                if let Some(ref q_path) = warning.quarantine_path {
                    ui.label(
                        egui::RichText::new(t!("Die beschädigte Originaldatei wurde in Quarantäne gesichert:"))
                            .size(12.0)
                            .color(egui::Color32::from_gray(220)),
                    );
                    ui.add_space(4.0);
                    ui.monospace(
                        egui::RichText::new(q_path.display().to_string())
                            .size(11.0)
                            .color(egui::Color32::from_rgb(255, 215, 0)),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(
                            t!("Um Datenverlust zu verhindern, ist diese Seite schreibgeschützt.\n\
                             Wenn Sie diese Seite bearbeiten möchten, können Sie den Schreibschutz aufheben."),
                        )
                        .size(11.5)
                        .color(egui::Color32::from_gray(180)),
                    );
                    ui.add_space(12.0);
                    if ui
                        .button(
                            egui::RichText::new(t!("Als leere Seite bearbeiten (Schreibschutz aufheben)"))
                                .color(egui::Color32::WHITE)
                                .size(12.5),
                        )
                        .clicked()
                    {
                        actions.push(UiAction::UnlockCorruptedPage(warning.page_id));
                    }
                } else {
                    ui.label(
                        egui::RichText::new(
                            t!("Zugriffsfehler (I/O). Die Datei existiert, kann aber nicht gelesen werden.\n\
                             Die Seite bleibt schreibgeschützt, um ein Überschreiben zu verhindern."),
                        )
                        .size(12.0)
                        .color(egui::Color32::from_gray(200)),
                    );
                }
            });
    }

    // Notfall-/Sicherheits-Dialog bei Fehlschlag des Speicherns vor dem Beenden
    if let Some(err) = &state.shutdown_error {
        let mut dismiss = false;
        let width = dialog_width(ctx, state, 460.0);
        dialog_window(t!("⚠ Ungespeicherte Änderungen"), state, true).show(ctx, |ui| {
            ui.set_width(width);
            touch_friendly_spacing(ui);
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "⚠");
                ui.label(
                    egui::RichText::new(t!("Fehler beim Beenden"))
                        .color(egui::Color32::from_rgb(255, 100, 100))
                        .size(16.0)
                        .strong(),
                );
            });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(t!(
                    "Ungespeicherte Änderungen konnten nicht gesichert werden:"
                ))
                .size(13.0)
                .color(egui::Color32::WHITE),
            );
            ui.add_space(6.0);
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(100))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    for msg in &err.messages {
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.5)
                                .color(egui::Color32::from_rgb(255, 180, 180)),
                        );
                    }
                });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(t!("Trotzdem beenden?"))
                    .size(13.0)
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button(t!("Abbrechen")).clicked() {
                    dismiss = true;
                }
                if ui.button(t!("Wiederholen")).clicked() {
                    actions.push(UiAction::RetryShutdown);
                }
                if ui.button(t!("Notfall-Export")).clicked() {
                    actions.push(UiAction::ExportRecoveryAndExit);
                }
                if ui
                    .button(
                        egui::RichText::new(t!("Trotzdem beenden"))
                            .color(egui::Color32::from_rgb(255, 100, 100)),
                    )
                    .clicked()
                {
                    actions.push(UiAction::ForceExit);
                }
            });
        });
        if dismiss {
            state.shutdown_error = None;
        }
    }
}

/// Pfad des gezeigten Ordners im Dokumenten-Manager: „📚 Alle › 3. Semester › Mathe“, jeder
/// Teil antippbar.
pub(super) fn folder_breadcrumbs(ui: &mut egui::Ui, state: &mut UiState, folder: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut target = None;
        if ui
            .selectable_label(
                folder.is_empty(),
                egui::RichText::new(t!("📚 Alle")).size(13.0),
            )
            .clicked()
        {
            target = Some(String::new());
        }
        let mut path = String::new();
        for name in folder.split('/').filter(|n| !n.is_empty()) {
            ui.label(egui::RichText::new("›").color(egui::Color32::from_gray(140)));
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(name);
            if ui
                .selectable_label(path == folder, egui::RichText::new(name).size(13.0))
                .clicked()
            {
                target = Some(path.clone());
            }
        }
        if let Some(target) = target {
            state.notebook_folder = target;
            state.renaming_folder = None;
            state.confirm_delete_folder = None;
        }
    });
}
/// Zeile eines Unterordners: antippen öffnet ihn; umbenennen und löschen (mit Rückfrage).
pub(super) fn folder_row(
    ui: &mut egui::Ui,
    state: &mut UiState,
    path: &str,
    actions: &mut Vec<UiAction>,
) {
    let name = crate::library::folder_name(path);
    if state.renaming_folder.as_deref() == Some(path) {
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.renaming_title_input)
                    .margin(egui::vec2(8.0, 8.0))
                    .desired_width(260.0),
            );
            if ui.button("✔").clicked()
                || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                actions.push(UiAction::RenameFolder {
                    path: path.to_string(),
                    new_name: state.renaming_title_input.trim().to_string(),
                });
                state.renaming_folder = None;
            }
            if ui.button("🗙").clicked() {
                state.renaming_folder = None;
            }
        });
        return;
    }
    if state.confirm_delete_folder.as_deref() == Some(path) {
        egui::Frame::NONE
            .fill(egui::Color32::from_rgba_unmultiplied(170, 50, 45, 40))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(t!(
                    "Ordner „{name}“ löschen? Seine Notizbücher und Unterordner rücken eine \
                     Ebene nach oben, gelöscht wird kein Notizbuch.",
                    name = name
                ))
                        .wrap(),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                egui::RichText::new(t!("Ordner löschen"))
                                    .color(egui::Color32::WHITE),
                            )
                            .clicked()
                        {
                            actions.push(UiAction::DeleteFolder(path.to_string()));
                            state.confirm_delete_folder = None;
                        }
                        if ui.button(t!("Abbrechen")).clicked() {
                            state.confirm_delete_folder = None;
                        }
                    });
                });
            });
        return;
    }

    let ids = state.available_notebooks.iter().map(|s| s.id.as_str());
    let count = state.library.count_within(path, ids);
    let mut open = false;
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 8))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 16),
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            // Erst die Knöpfe (rechts), der Name füllt den Rest und öffnet den Ordner
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(
                        egui::RichText::new("🗑")
                            .size(16.0)
                            .color(egui::Color32::from_rgb(255, 120, 120)),
                    )
                    .on_hover_text(t!("Ordner löschen (die Notizbücher bleiben erhalten)"))
                    .clicked()
                {
                    state.confirm_delete_folder = Some(path.to_string());
                }
                if ui
                    .button(egui::RichText::new("📂").size(16.0))
                    .on_hover_text(t!("Ordner samt Inhalt in einen anderen Ordner verschieben"))
                    .clicked()
                {
                    state.moving = Some(MoveSubject::Folder(path.to_string()));
                }
                if ui
                    .button(egui::RichText::new("✏").size(16.0))
                    .on_hover_text(t!("Ordner umbenennen"))
                    .clicked()
                {
                    state.renaming_folder = Some(path.to_string());
                    state.renaming_title_input = name.to_string();
                }
                let title_area = ui
                    .with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.vertical(|ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(
                                egui::RichText::new(format!("📁 {name}"))
                                    .strong()
                                    .size(13.0)
                                    .color(egui::Color32::WHITE),
                            );
                            ui.label(
                                egui::RichText::new(match count {
                                    1 => t!("1 Notizbuch").to_string(),
                                    n => t!("{n} Notizbücher", n = n),
                                })
                                .size(11.0)
                                .color(egui::Color32::from_gray(160)),
                            );
                        })
                    })
                    .inner;
                open = title_area
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
            });
        });
    if open {
        state.notebook_folder = path.to_string();
    }
}
/// Zielordner für das Verschieben eines Notizbuchs oder Ordners: oberste Ebene und alle
/// Ordner als Baum (ein Ordner nicht in sich selbst oder seine Unterordner).
pub(super) fn move_target_list(
    ui: &mut egui::Ui,
    state: &mut UiState,
    subject: &MoveSubject,
    actions: &mut Vec<UiAction>,
) {
    let (title, current, folders) = match subject {
        MoveSubject::Notebook(id) => (
            state
                .available_notebooks
                .iter()
                .find(|s| &s.id == id)
                .map_or(id.as_str(), |s| s.title.as_str())
                .to_string(),
            state.library.folder_of(id).to_string(),
            state.library.all_folders(),
        ),
        MoveSubject::Folder(path) => (
            format!("📁 {}", crate::library::folder_name(path)),
            crate::library::parent_folder(path).to_string(),
            state.library.folder_move_targets(path),
        ),
    };
    ui.label(egui::RichText::new(t!("„{title}“ verschieben nach:", title = title)).strong());
    ui.add_space(4.0);
    let mut targets = vec![(String::new(), t!("📚 Oberste Ebene").to_string(), 0)];
    for path in folders {
        let depth = path.matches('/').count() + 1;
        targets.push((
            path.to_string(),
            format!("📁 {}", crate::library::folder_name(path)),
            depth,
        ));
    }
    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            for (path, label, depth) in targets {
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * 18.0);
                    let here = path == current;
                    if ui
                        .add_enabled(!here, egui::Button::new(label))
                        .on_disabled_hover_text(t!("Liegt bereits hier"))
                        .clicked()
                    {
                        actions.push(match subject {
                            MoveSubject::Notebook(id) => UiAction::MoveNotebook {
                                id: id.clone(),
                                folder: path.clone(),
                            },
                            MoveSubject::Folder(folder) => UiAction::MoveFolder {
                                path: folder.clone(),
                                new_parent: path.clone(),
                            },
                        });
                        state.moving = None;
                    }
                });
            }
        });
    ui.add_space(6.0);
    if ui.button(t!("Abbrechen")).clicked() {
        state.moving = None;
    }
}
/// Dialog „Bild einfügen“ bzw. „PDF importieren“: Schnellzugriffe, Ordnerpfad und Liste aus
/// Ordnern und passenden Dateien.
pub(super) fn file_picker_dialog(
    ctx: &egui::Context,
    state: &mut UiState,
    actions: &mut Vec<UiAction>,
) {
    let mut close = false;
    let mut navigate: Option<std::path::PathBuf> = None;
    let mut go_up = false;
    let mut chosen: Option<std::path::PathBuf> = None;

    let Some(picker) = state.file_picker.as_ref() else {
        return;
    };
    let picker_kind = picker.kind;
    let (title, header, file_icon, empty_hint, tip) = match picker.kind {
        PickerKind::Image => (
            t!("Bild einfügen"),
            t!("🖼 Bild einfügen"),
            "🖼",
            t!("Keine Ordner oder Bilder (PNG, JPEG, GIF, WebP, BMP)"),
            t!("Tipp: Screenshots mit Strg+V einfügen oder Bilddateien ins Fenster ziehen."),
        ),
        PickerKind::Import => (
            t!("Importieren"),
            t!("📄 Importieren"),
            "📄",
            t!("Keine Ordner oder importierbaren Dateien (PDF, Xournal++, Rnote, Saber)"),
            t!(
                "Die Datei wird als neues Notizbuch angelegt. PDF: eine Seite je PDF-Seite, \
             beim Export liegen deine Notizen über den Originalseiten. \
             Xournal++ (.xopp, .xoj), Rnote (.rnote) und Saber (.sbn2, .sba): Striche, Formen, \
             Texte und Bilder werden übernommen, bei Xournal++ und Saber auch PDF-Hintergründe."
            ),
        ),
        PickerKind::Folder => (
            t!("Speicherort wählen"),
            t!("📁 Speicherort wählen"),
            "📁",
            t!("Keine Unterordner"),
            t!(
                "Die Notizbücher liegen dann im Unterordner „ink-notes“ des gewählten Ordners, \
             z. B. im Nextcloud- oder Google-Drive-Ordner. Ist er noch leer, werden die \
             vorhandenen Notizbücher dorthin kopiert; die alten bleiben erhalten."
            ),
        ),
    };
    let width = dialog_width(ctx, state, 460.0);
    dialog_window(title, state, false).show(ctx, |ui| {
        ui.set_width(width);
        touch_friendly_spacing(ui);
        if dialog_header(ui, header, true) {
            close = true;
        }

        ui.horizontal_wrapped(|ui| {
            for (label, path) in crate::file_picker::shortcuts() {
                if ui
                    .selectable_label(picker.dir == path, crate::i18n::tr(label))
                    .clicked()
                {
                    navigate = Some(path);
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(picker.dir.parent().is_some(), egui::Button::new("⏶"))
                .on_hover_text(t!("Übergeordneter Ordner"))
                .clicked()
            {
                go_up = true;
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(picker.dir.display().to_string())
                        .color(egui::Color32::from_gray(190)),
                )
                .truncate(),
            );
        });
        if let Some(err) = &picker.error {
            ui.colored_label(egui::Color32::from_rgb(255, 120, 120), err);
        }
        if picker_kind == PickerKind::Folder
            && ui
                .add_sized(
                    [ui.available_width(), 30.0],
                    egui::Button::new(t!("✔  Diesen Ordner wählen")),
                )
                .clicked()
        {
            chosen = Some(picker.dir.clone());
        }
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .max_height((ctx.viewport_rect().height() - 330.0).clamp(160.0, 420.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if picker.entries.is_empty() {
                    ui.label(egui::RichText::new(empty_hint).color(egui::Color32::from_gray(150)));
                }
                for entry in &picker.entries {
                    let label = if entry.is_dir {
                        format!("📁  {}", entry.name)
                    } else {
                        format!("{file_icon}  {}", entry.name)
                    };
                    let button = egui::Button::new(label)
                        .frame(false)
                        .min_size(egui::vec2(ui.available_width(), 34.0));
                    let response = ui.add(button);
                    if response.clicked() {
                        if entry.is_dir {
                            navigate = Some(entry.path.clone());
                        } else {
                            chosen = Some(entry.path.clone());
                        }
                    }
                }
            });

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tip)
                .size(11.5)
                .color(egui::Color32::from_gray(150)),
        );
    });

    if let Some(picker) = state.file_picker.as_mut() {
        if go_up {
            picker.go_up();
        }
        if let Some(dir) = navigate {
            picker.navigate(&dir);
        }
    }
    if let Some(path) = chosen {
        if picker_kind != PickerKind::Folder {
            state.last_picker_dir = state.file_picker.as_ref().map(|p| p.dir.clone());
        }
        actions.push(match picker_kind {
            PickerKind::Image => UiAction::InsertImageFile(path),
            PickerKind::Import => UiAction::ImportFile(path),
            PickerKind::Folder => UiAction::SetStorageDir(Some(path)),
        });
        close = true;
    }
    if close {
        state.file_picker = None;
    }
}
