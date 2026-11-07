//! The Terrain pane: the terrain blend preview (`crate::terrain`) with its
//! slots, ground and settings. It renders only while the pane is shown.

use super::PainterApp;
use crate::terrain::{self, LoadedMaterial, SLOT_ROLES, SLOTS, SlotSource, TerrainPreview};
use crate::theme;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct TerrainState {
    pub preview: Option<TerrainPreview>,
    /// Stamps of the stamps folder (`Prefs::terrain_stamps`), and the folder they came from.
    stamps: Vec<terrain::Stamp>,
    stamps_read: Option<String>,
    /// The engine generation the live slot was last built from, and when a newer one appeared.
    live_built: u64,
    live_pending_since: Option<Instant>,
    live_job: Option<mpsc::Receiver<Result<LoadedMaterial, String>>>,
    pub status: String,
    hide_controls: bool,
}

/// What a click in the controls asks for, applied after the UI pass.
enum Action {
    Slot(usize, SlotSource),
    PickSlot(usize),
    Stamp(usize),
    Sample,
    PickHeight,
    PickStamps,
}

impl PainterApp {
    fn terrain_preview(&mut self) -> &mut TerrainPreview {
        self.terrain.preview.get_or_insert_with(|| TerrainPreview::new(&self.rs))
    }

    /// Builds the open document's preview maps (the game layout, 1024 px).
    pub(super) fn live_material_job(&mut self) -> impl FnOnce() -> Result<LoadedMaterial, String> + Send + 'static {
        if self.engine.dirty {
            self.engine.composite();
        }
        let snapshot = crate::export::snapshot_for_export(&mut self.engine, &self.rs.device, &self.rs.queue, self.mesh.clone());
        let mut settings = self.export_settings();
        settings.size = Some(terrain::MAT_SIZE);
        settings.padding = crate::export::Padding::None;
        settings.format = crate::export::Format::Png;
        let f = &self.engine.features;
        let info = terrain::SlotInfo {
            name: self.project_path.as_ref().and_then(|p| p.file_stem()).map_or("Document".into(), |s| s.to_string_lossy().into_owned()),
            tile_m: f.tile_m,
            relief_m: f.relief_m,
        };
        move || {
            let maps = crate::export::build_maps(&snapshot, &crate::export::preview_template(), &settings)?;
            let to8 = |img: &image::ImageBuffer<image::Rgba<f32>, Vec<f32>>| {
                image::RgbaImage::from_fn(img.width(), img.height(), |x, y| {
                    let p = img.get_pixel(x, y);
                    image::Rgba(p.0.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8))
                })
            };
            Ok(LoadedMaterial { color_height: to8(&maps[0]), normal_masks: to8(&maps[1]), info })
        }
    }

    /// Keeps a "Live document" slot following the document: rebuilt on a
    /// worker thread once edits have paused for a moment.
    fn update_terrain_live(&mut self) {
        if let Some(rx) = &self.terrain.live_job {
            match rx.try_recv() {
                Ok(Ok(m)) => {
                    self.terrain.live_job = None;
                    let rs = self.rs.clone();
                    let t = self.terrain_preview();
                    for k in 0..SLOTS {
                        if t.sources[k] == SlotSource::Live {
                            t.set_slot(&rs.queue, k, &m);
                        }
                    }
                }
                Ok(Err(e)) => {
                    self.terrain.live_job = None;
                    self.terrain.status = format!("Live material: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.terrain.live_job = None,
            }
        }
        let wants_live = self.terrain.preview.as_ref().is_some_and(|t| t.sources.contains(&SlotSource::Live));
        if !wants_live || self.engine.generation == self.terrain.live_built {
            self.terrain.live_pending_since = None;
            return;
        }
        let since = *self.terrain.live_pending_since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_millis(600) && self.terrain.live_built != 0 {
            return;
        }
        self.terrain.live_built = self.engine.generation;
        self.terrain.live_pending_since = None;
        let job = self.live_material_job();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(job());
        });
        self.terrain.live_job = Some(rx);
    }

    /// Puts `source` in slot `k` now (live: built on the spot).
    pub(super) fn set_terrain_slot(&mut self, k: usize, source: SlotSource) -> Result<(), String> {
        let rs = self.rs.clone();
        match &source {
            SlotSource::Empty => self.terrain_preview().clear_slot(k),
            SlotSource::Live => {
                let m = (self.live_material_job())()?;
                self.terrain.live_built = self.engine.generation;
                let t = self.terrain_preview();
                t.set_slot(&rs.queue, k, &m);
                t.sources[k] = SlotSource::Live;
            }
            SlotSource::File(p) => {
                let m = terrain::load_material(p)?;
                let t = self.terrain_preview();
                t.set_slot(&rs.queue, k, &m);
                t.sources[k] = source.clone();
            }
        }
        Ok(())
    }

    pub(super) fn terrain_stamps(&mut self) -> &[terrain::Stamp] {
        let folder = self.prefs.terrain_stamps.clone();
        if self.terrain.stamps_read.as_deref() != Some(folder.as_str()) {
            self.terrain.stamps = if folder.is_empty() { Vec::new() } else { terrain::read_stamps(Path::new(&folder)) };
            self.terrain.stamps_read = Some(folder);
        }
        &self.terrain.stamps
    }

    /// Uses a stamp (by index in `terrain_stamps`) as the ground.
    pub(super) fn set_terrain_stamp(&mut self, i: usize) -> Result<(), String> {
        let s = self.terrain_stamps().get(i).cloned().ok_or("no such stamp")?;
        let img = terrain::load_height(&s.path)?;
        let rs = self.rs.clone();
        let name = format!("{} ({})", s.name, s.category);
        self.terrain_preview().set_height(&rs.queue, &img, terrain::Ground { name, footprint_m: s.footprint_m, amplitude_m: s.amplitude_m });
        Ok(())
    }

    pub(super) fn set_terrain_heightmap(&mut self, path: &Path, footprint_m: f32, amplitude_m: f32) -> Result<(), String> {
        let img = terrain::load_height(path)?;
        let rs = self.rs.clone();
        let name = path.file_stem().map_or("heightmap".into(), |s| s.to_string_lossy().into_owned());
        self.terrain_preview().set_height(&rs.queue, &img, terrain::Ground { name, footprint_m, amplitude_m });
        Ok(())
    }

    pub(super) fn terrain_pane(&mut self, ui: &mut egui::Ui) {
        self.terrain_preview();
        self.update_terrain_live();
        let rect = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        let size = [(rect.width() * ppp) as u32, (rect.height() * ppp) as u32];
        let rs = self.rs.clone();
        let t = self.terrain.preview.as_mut().expect("made above");
        let id = t.render(&rs, size);
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        t.handle_input(ui, &resp);
        if self.terrain.live_job.is_some() || self.terrain.live_pending_since.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        let panel = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(300.0, (rect.height() - 16.0).max(40.0)));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(panel));
        let actions = self.terrain_controls(&mut child);
        for a in actions {
            let r = match a {
                Action::Slot(k, s) => self.set_terrain_slot(k, s),
                Action::PickSlot(k) => match rfd::FileDialog::new().add_filter("Game material", &["json"]).set_title("A game export's material.json").pick_file() {
                    Some(p) => self.set_terrain_slot(k, SlotSource::File(p)),
                    None => Ok(()),
                },
                Action::Stamp(i) => self.set_terrain_stamp(i),
                Action::Sample => {
                    let img = image::load_from_memory(include_bytes!("../../assets/terrain/sample_height.png")).map_err(|e| e.to_string());
                    img.map(|img| {
                        let rs = self.rs.clone();
                        self.terrain_preview().set_height(&rs.queue, &img, terrain::Ground { name: "Sample".into(), footprint_m: 160.0, amplitude_m: 9.0 });
                    })
                }
                Action::PickHeight => match rfd::FileDialog::new().add_filter("Heightmap", &["png", "tif", "tiff", "exr", "jpg"]).pick_file() {
                    Some(p) => {
                        let (f, a) = self.terrain.preview.as_ref().map_or((160.0, 9.0), |t| (t.ground.footprint_m, t.ground.amplitude_m));
                        self.set_terrain_heightmap(&p, f, a)
                    }
                    None => Ok(()),
                },
                Action::PickStamps => {
                    if let Some(dir) = rfd::FileDialog::new().set_title("A landscape stamp folder (with Library.json)").pick_folder() {
                        self.prefs.terrain_stamps = dir.display().to_string();
                        let _ = self.prefs.save();
                    }
                    Ok(())
                }
            };
            self.terrain.status = match r {
                Ok(()) => String::new(),
                Err(e) => e,
            };
        }
    }

    fn terrain_controls(&mut self, ui: &mut egui::Ui) -> Vec<Action> {
        let mut actions = Vec::new();
        let stamps: Vec<(String, String)> = self.terrain_stamps().iter().map(|s| (s.category.clone(), s.name.clone())).collect();
        let hide = &mut self.terrain.hide_controls;
        let status = self.terrain.status.clone();
        let t = self.terrain.preview.as_mut().expect("made before the controls");
        egui::Frame::new().fill(theme::PANEL.gamma_multiply(0.92)).corner_radius(4.0).inner_margin(8.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("TERRAIN BLEND").small().strong());
                if ui.small_button(if *hide { "Show" } else { "Hide" }).clicked() {
                    *hide = !*hide;
                }
            });
            if *hide {
                return;
            }
            egui::ScrollArea::vertical().max_height(ui.available_height() - 8.0).show(ui, |ui| {
                ui.label(egui::RichText::new("Materials, by where the game's splat puts them").small().color(theme::TEXT_DIM));
                for k in 0..SLOTS {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(SLOT_ROLES[k]).small());
                    });
                    ui.horizontal(|ui| {
                        let shown = match (&t.sources[k], &t.slots[k]) {
                            (SlotSource::Empty, _) | (_, None) => "Empty".to_string(),
                            (SlotSource::Live, Some(i)) => format!("Live: {}", i.name),
                            (SlotSource::File(_), Some(i)) => format!("{} ({} m)", i.name, i.tile_m),
                        };
                        egui::ComboBox::from_id_salt(("terrain slot", k)).width(190.0).selected_text(shown).show_ui(ui, |ui| {
                            if ui.selectable_label(t.sources[k] == SlotSource::Empty, "Empty").clicked() {
                                actions.push(Action::Slot(k, SlotSource::Empty));
                            }
                            if ui.selectable_label(t.sources[k] == SlotSource::Live, "Live document").on_hover_text("The open document, rebuilt as you edit").clicked() {
                                actions.push(Action::Slot(k, SlotSource::Live));
                            }
                        });
                        if ui.small_button("Load…").on_hover_text("A game export's <name>.material.json").clicked() {
                            actions.push(Action::PickSlot(k));
                        }
                    });
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("Ground: {} ({:.0} m, {:.0} m high)", t.ground.name, t.ground.footprint_m, t.ground.amplitude_m)).small());
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("terrain stamp").width(190.0).selected_text("Heightmap…").height(420.0).show_ui(ui, |ui| {
                        if ui.selectable_label(false, "Sample").clicked() {
                            actions.push(Action::Sample);
                        }
                        let mut last = String::new();
                        for (i, (cat, name)) in stamps.iter().enumerate() {
                            if *cat != last {
                                ui.label(egui::RichText::new(cat.to_uppercase()).small().color(theme::TEXT_DIM));
                                last = cat.clone();
                            }
                            if ui.selectable_label(false, name).clicked() {
                                actions.push(Action::Stamp(i));
                            }
                        }
                    });
                    if ui.small_button("File…").clicked() {
                        actions.push(Action::PickHeight);
                    }
                    if ui.small_button("Stamps…").on_hover_text("A landscape stamp folder with a Library.json").clicked() {
                        actions.push(Action::PickStamps);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut t.ground.footprint_m).range(4.0..=4000.0).suffix(" m").prefix("size "));
                    ui.add(egui::DragValue::new(&mut t.ground.amplitude_m).range(0.0..=500.0).suffix(" m").prefix("height "));
                });
                ui.separator();
                let s = &mut t.settings;
                ui.add(egui::Slider::new(&mut s.blend_depth, 0.01..=0.5).text("blend depth")).on_hover_text("The game's look.y: how far below the highest surface another still shows");
                ui.add(egui::Slider::new(&mut s.splat_scale, 0.1..=4.0).text("splat scale"));
                ui.checkbox(&mut s.anti_tiling, "Anti-tiling");
                ui.checkbox(&mut s.show_weights, "Show blend weights");
                ui.add(egui::Slider::new(&mut s.sun_azimuth, -180.0..=180.0).text("sun azimuth"));
                ui.add(egui::Slider::new(&mut s.sun_elevation, 2.0..=89.0).text("sun elevation"));
                ui.add(egui::Slider::new(&mut s.exposure, 0.3..=3.0).text("exposure"));
                ui.horizontal(|ui| {
                    for (label, name) in [("Player", "player"), ("Close", "close"), ("Overview", "overview")] {
                        if ui.small_button(label).clicked() {
                            t.camera_preset(name);
                        }
                    }
                });
                if !status.is_empty() {
                    ui.label(egui::RichText::new(status).small().color(egui::Color32::from_rgb(240, 120, 90)));
                }
            });
        });
        actions
    }
}

/// A slot argument from the CLI: "live", "empty"/null, or a material.json path.
pub(super) fn slot_source(v: &serde_json::Value) -> Result<SlotSource, String> {
    match v {
        serde_json::Value::Null => Ok(SlotSource::Empty),
        serde_json::Value::String(s) if s.eq_ignore_ascii_case("live") => Ok(SlotSource::Live),
        serde_json::Value::String(s) if s.is_empty() || s.eq_ignore_ascii_case("empty") => Ok(SlotSource::Empty),
        serde_json::Value::String(s) => Ok(SlotSource::File(PathBuf::from(s))),
        _ => Err("a slot is \"live\", null, or a path to a material.json".into()),
    }
}
