//! Mesh sets: several meshes painted with one layer stack, one shown at a
//! time. Each member has its own mesh (and auto smooth), mesh maps (baked
//! on switch, usually from the bake cache) and UV-bound data: paint and
//! paint masks. Fills, generators, graphs and settings are shared, so a
//! change to the stack shows on every member. Switching swaps the mesh and
//! the UV data, so it costs one mesh install, never all members at once.

use super::{PainterApp, theme};
use crate::engine::uv_data::UvData;
use crate::mesh::Mesh;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct Member {
    pub id: u64,
    pub name: String,
    /// The file it came from, when there is one.
    pub path: Option<PathBuf>,
    /// The mesh as imported (the shown member's lives in `mesh_source`).
    pub source: Arc<Mesh>,
    pub auto_smooth: Option<f32>,
    /// The member's paint and masks while another member is shown (empty
    /// for the shown one, whose data is in the engine).
    pub uv: UvData,
}

#[derive(Default)]
pub(super) struct MeshSet {
    /// Empty: no mesh set, just the one mesh.
    pub members: Vec<Member>,
    pub active: usize,
    pub next_id: u64,
    /// Members added, removed or switched since the last save (switching
    /// clears history, so the history serial alone can't tell).
    pub modified: bool,
}

impl MeshSet {
    pub fn is_on(&self) -> bool {
        !self.members.is_empty()
    }
}

impl PainterApp {
    /// Adds meshes to the mesh set, starting one with the current mesh if
    /// there is none. Each needs a single texture set, like the current mesh.
    pub(super) fn mesh_set_add(&mut self, meshes: Vec<(Mesh, Option<PathBuf>)>) -> Result<usize, String> {
        if self.mesh.sets.len() != 1 {
            return Err("Mesh sets need a mesh with one texture set.".into());
        }
        for (m, _) in &meshes {
            if m.sets.len() != 1 {
                return Err(format!("'{}' has {} texture sets; mesh set members need one.", m.name, m.sets.len()));
            }
        }
        if !self.mesh_set.is_on() {
            let id = self.mesh_set.next_id;
            self.mesh_set.next_id += 1;
            self.mesh_set.members.push(Member {
                id,
                name: self.mesh_source.name.clone(),
                path: None,
                source: self.mesh_source.clone(),
                auto_smooth: self.auto_smooth,
                uv: UvData::default(),
            });
            self.mesh_set.active = 0;
        }
        let n = meshes.len();
        for (mesh, path) in meshes {
            let id = self.mesh_set.next_id;
            self.mesh_set.next_id += 1;
            let name = path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| mesh.name.clone());
            // The shown member's auto smooth carries over: one look for the set.
            let auto_smooth = self.auto_smooth;
            self.mesh_set.members.push(Member { id, name, path, source: Arc::new(mesh), auto_smooth, uv: UvData::default() });
        }
        self.mesh_set.modified = true;
        self.status = format!("Mesh set: {} members.", self.mesh_set.members.len());
        Ok(n)
    }

    /// Shows member `k`: its mesh, mesh maps and paint, under the shared stack.
    pub(super) fn mesh_set_switch(&mut self, k: usize, ctx: &egui::Context) {
        let ms = &self.mesh_set;
        if k == ms.active || k >= ms.members.len() || self.stroke.is_some() {
            return;
        }
        self.end_preview(true);
        self.engine.commit_props(&mut self.undo);
        // History is per mesh: it ends here, so remember whether it held unsaved work.
        let modified = self.is_modified();
        let incoming = std::mem::take(&mut self.mesh_set.members[k].uv);
        let outgoing = self.engine.swap_uv_data(incoming);
        let old = self.mesh_set.active;
        let out = &mut self.mesh_set.members[old];
        out.uv = outgoing;
        out.source = self.mesh_source.clone();
        out.auto_smooth = self.auto_smooth;
        let m = &self.mesh_set.members[k];
        self.mesh_source = m.source.clone();
        self.auto_smooth = m.auto_smooth;
        self.saved_auto_smooth = self.auto_smooth;
        if let Some(a) = self.auto_smooth {
            self.auto_smooth_angle = a;
        }
        self.mesh_set.active = k;
        self.mesh_set.modified |= modified;
        self.undo.clear();
        self.saved_serial = self.undo.state_serial();
        self.install_mesh(false, ctx);
        for d in &mut self.graphs.docs {
            d.dirty = true;
        }
        let m = &self.mesh_set.members[k];
        self.status = format!("Mesh set: {} ({}/{}).", m.name, k + 1, self.mesh_set.members.len());
    }

    /// Removes member `k` (showing a neighbour first when it's the shown
    /// one). Its paint goes with it.
    pub(super) fn mesh_set_remove(&mut self, k: usize, ctx: &egui::Context) {
        let n = self.mesh_set.members.len();
        if k >= n || self.stroke.is_some() {
            return;
        }
        if n == 1 {
            self.mesh_set_clear();
            return;
        }
        if k == self.mesh_set.active {
            self.mesh_set_switch(if k + 1 < n { k + 1 } else { k - 1 }, ctx);
        }
        let m = self.mesh_set.members.remove(k);
        if self.mesh_set.active > k {
            self.mesh_set.active -= 1;
        }
        self.mesh_set.modified = true;
        self.status = format!("Removed '{}' from the mesh set.", m.name);
    }

    /// Ends the mesh set, keeping only the shown mesh and its paint.
    pub(super) fn mesh_set_clear(&mut self) {
        if self.mesh_set.is_on() {
            self.mesh_set = MeshSet { modified: true, ..Default::default() };
            self.status = "Mesh set cleared: the shown mesh stays.".into();
        }
    }

    pub(super) fn mesh_set_add_dialog(&mut self) {
        let Some(paths) = rfd::FileDialog::new().add_filter("Mesh (FBX, OBJ)", &crate::import::EXTENSIONS).pick_files() else {
            return;
        };
        let mut meshes = Vec::new();
        for p in paths {
            match Mesh::load_file(&p) {
                Ok(m) => meshes.push((m, Some(p))),
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        }
        if let Err(e) = self.mesh_set_add(meshes) {
            self.status = e;
        }
    }

    /// The Mesh Set tab: the meshes painted with this layer stack, one row
    /// each like the layer list; click a row to show that mesh.
    pub(super) fn mesh_set_panel(&mut self, ui: &mut egui::Ui) {
        let n = self.mesh_set.members.len();
        let mut switch = None;
        let mut remove = None;
        ui.horizontal(|ui| {
            let single = self.mesh.sets.len() == 1;
            let tip = "Add meshes painted with this layer stack, one shown at a time. Each keeps its own paint, paint masks and mesh maps.";
            if ui.add_enabled(single, egui::Button::new(format!("{}  Add meshes...", crate::icons::PLUS))).on_hover_text(tip).clicked() {
                self.mesh_set_add_dialog();
            }
            if n > 1 {
                let a = self.mesh_set.active;
                if ui.button("<").on_hover_text("Previous mesh").clicked() {
                    switch = Some((a + n - 1) % n);
                }
                if ui.button(">").on_hover_text("Next mesh").clicked() {
                    switch = Some((a + 1) % n);
                }
            }
            if n > 0 && ui.button("Clear").on_hover_text("End the mesh set; the shown mesh and its paint stay").clicked() {
                self.mesh_set_clear();
            }
        });
        ui.add_space(6.0);
        if self.mesh.sets.len() != 1 {
            ui.label(egui::RichText::new("Mesh sets need a mesh with one texture set.").color(theme::TEXT_DIM));
            return;
        }
        if n == 0 {
            ui.label(
                egui::RichText::new(
                    "Paint several meshes with one layer stack: fills, smart masks and settings are shared,                      while each mesh keeps its own paint and mesh maps. One mesh is loaded at a time.",
                )
                .color(theme::TEXT_DIM),
            );
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (k, m) in self.mesh_set.members.iter().enumerate() {
                let shown = k == self.mesh_set.active;
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
                let bg = if shown {
                    theme::ACCENT_DIM
                } else if resp.hovered() {
                    theme::WIDGET_HOVER
                } else {
                    theme::WIDGET
                };
                let p = ui.painter();
                p.rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 0.0, bg);
                if shown {
                    p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, theme::ACCENT);
                }
                let icon = egui::FontId::proportional(18.0);
                p.text(rect.left_center() + egui::vec2(14.0, 0.0), egui::Align2::LEFT_CENTER, crate::icons::CUBE_TRANSPARENT, icon, theme::TEXT_DIM);
                p.text(rect.left_center() + egui::vec2(42.0, -8.0), egui::Align2::LEFT_CENTER, &m.name, egui::FontId::proportional(13.0), theme::TEXT);
                let mut info = format!("{} triangles", m.source.triangle_count());
                if let Some(a) = m.auto_smooth {
                    info += &format!(" · auto smooth {a:.0}°");
                }
                p.text(rect.left_center() + egui::vec2(42.0, 10.0), egui::Align2::LEFT_CENTER, info, egui::FontId::proportional(11.0), theme::TEXT_DIM);
                let x = egui::Rect::from_center_size(rect.right_center() - egui::vec2(18.0, 0.0), egui::vec2(24.0, 24.0));
                let xr = ui.interact(x, ui.scope_id().with(("mesh_set_x", m.id)), egui::Sense::click());
                let col = if xr.hovered() { theme::TEXT } else { theme::TEXT_DIM };
                ui.painter().text(x.center(), egui::Align2::CENTER_CENTER, crate::icons::X, egui::FontId::proportional(14.0), col);
                if xr.on_hover_text("Remove from the mesh set (its paint goes with it)").clicked() {
                    remove = Some(k);
                } else {
                    let tip = match &m.path {
                        Some(p) => format!("{}
Click to show this mesh.", p.display()),
                        None => "Click to show this mesh.".to_string(),
                    };
                    if resp.on_hover_text(tip).clicked() {
                        switch = Some(k);
                    }
                }
            }
        });
        let ctx = ui.ctx().clone();
        if let Some(k) = remove {
            self.mesh_set_remove(k, &ctx);
        } else if let Some(k) = switch {
            self.mesh_set_switch(k, &ctx);
        }
    }
}
