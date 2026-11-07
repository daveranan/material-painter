//! Texture sets: one document (layer stack, resolution, mesh maps) per
//! material of the mesh. The set being edited lives in `PainterApp::engine`
//! (with `selected` and `target`); the others wait here until switched to.
//! History is shared: undoing a step of another set switches to it first.

use super::{PainterApp, theme};
use crate::engine::Engine;
use crate::undo::{StrokeTarget, UndoStack};
use crate::viewport::OverlaySlot;
use eframe::egui;
use std::collections::VecDeque;

/// A texture set that isn't being edited, with the panel state it had.
pub(super) struct Parked {
    pub engine: Engine,
    pub selected: usize,
    pub target: StrokeTarget,
}

#[derive(Default)]
pub(super) struct TextureSets {
    /// The set in `PainterApp::engine`, by position in `Mesh::sets`.
    pub active: usize,
    /// One slot per mesh set; `None` for the active one.
    pub parked: Vec<Option<Parked>>,
    /// The set the running bake is for.
    pub baking: usize,
    /// Sets to bake after it ("Bake all").
    pub bake_queue: VecDeque<usize>,
}

/// The engines of `count` sets: `active` (the edited one) and the parked
/// ones. Borrows fields only, so a view can render while they're borrowed.
pub(super) fn engines<'a>(active: &'a Engine, sets: &'a TextureSets, count: usize) -> Vec<&'a Engine> {
    (0..count)
        .map(|k| match sets.parked.get(k).and_then(Option::as_ref) {
            Some(p) => &p.engine,
            None => active,
        })
        .collect()
}

impl PainterApp {
    /// Every set's engine, in mesh order (what the 3D view draws).
    pub(super) fn set_engines(&self) -> Vec<&Engine> {
        engines(&self.engine, &self.sets, self.mesh.sets.len())
    }

    /// Set `k`'s engine, active or parked.
    pub(super) fn set_engine_mut(&mut self, k: usize) -> Option<&mut Engine> {
        if k == self.sets.active {
            return Some(&mut self.engine);
        }
        self.sets.parked.get_mut(k)?.as_mut().map(|p| &mut p.engine)
    }

    /// Makes set `k` the one being edited.
    pub(super) fn switch_set(&mut self, k: usize) {
        if k == self.sets.active || self.stroke.is_some() {
            return;
        }
        let Some(mut p) = self.sets.parked.get_mut(k).and_then(Option::take) else { return };
        // A bake preview shows on the set being edited; the full bake comes back first.
        self.end_preview(true);
        self.engine.commit_props(&mut self.undo);
        std::mem::swap(&mut self.engine, &mut p.engine);
        std::mem::swap(&mut self.selected, &mut p.selected);
        std::mem::swap(&mut self.target, &mut p.target);
        self.sets.parked[self.sets.active] = Some(p);
        self.sets.active = k;
        self.undo.set = k;
        // Polygon fill selections are triangles of the set that was edited.
        self.poly.shown = None;
        self.poly.filled.clear();
        self.overlays.clear(OverlaySlot::Selection);
        // Graphs evaluate on the edited set's mesh maps.
        for d in &mut self.graphs.docs {
            d.dirty = true;
        }
        self.status = format!("Texture set: {}.", self.mesh.sets[k].name);
    }

    /// Before an undo or redo: the step belongs to `set`, which becomes the
    /// edited one so the step applies to its layers.
    pub(super) fn switch_for_history(&mut self, redo: bool) {
        let stack = if redo { &self.undo.redo } else { &self.undo.undo };
        // Graphs are shared by every set: their steps stay on the edited one.
        if let Some(set) = stack.last().filter(|e| !e.edit.graphs_only()).map(|e| e.set) {
            self.switch_set(set);
        }
    }

    /// Composites the parked sets that changed (new mesh, bake, load).
    pub(super) fn composite_parked(&mut self) {
        for p in self.sets.parked.iter_mut().flatten() {
            if p.engine.dirty {
                p.engine.composite();
            }
        }
    }

    /// After the mesh changed (`old_names`: the previous sets): every set
    /// keeps its layers when a new set has its name, else the next unused
    /// one in order; new sets start with one paint layer. The edited set
    /// stays edited when it survives.
    pub(super) fn rebuild_sets(&mut self, old_names: &[String]) {
        let new: Vec<(String, std::ops::Range<u32>)> =
            self.mesh.sets.iter().map(|s| (s.name.clone(), s.index_range())).collect();
        let mut used = vec![false; old_names.len()];
        let mut assign: Vec<Option<usize>> = new
            .iter()
            .map(|(name, _)| {
                let j = old_names.iter().enumerate().position(|(j, n)| !used[j] && n == name)?;
                used[j] = true;
                Some(j)
            })
            .collect();
        for a in assign.iter_mut().filter(|a| a.is_none()) {
            if let Some(j) = used.iter().position(|u| !u) {
                used[j] = true;
                *a = Some(j);
            }
        }
        let old_active = self.sets.active;
        let mut old = std::mem::take(&mut self.sets.parked);
        old.resize_with(old_names.len().max(old_active + 1), || None);
        let active = assign.iter().position(|a| *a == Some(old_active)).unwrap_or(0);
        let res = self.prefs.default_resolution;
        let fresh = |engine: &Engine, range: std::ops::Range<u32>, mesh: &crate::gpu::GpuMesh| {
            let mut e = engine.sibling(mesh, range, res);
            e.add_paint_layer(0, None, None, &mut UndoStack::default());
            Parked { engine: e, selected: 0, target: StrokeTarget::Content }
        };
        let mut parked = Vec::with_capacity(new.len());
        for (i, (_, range)) in new.iter().enumerate() {
            if i == active {
                parked.push(None);
                continue;
            }
            let kept = assign[i].filter(|&j| j != old_active).and_then(|j| old[j].take());
            let p = match kept {
                Some(p) => p,
                None => fresh(&self.engine, range.clone(), &self.gpu_mesh),
            };
            parked.push(Some(p));
        }
        if assign[active] != Some(old_active) {
            let p = match assign[active].and_then(|j| old[j].take()) {
                Some(p) => p,
                None => fresh(&self.engine, new[active].1.clone(), &self.gpu_mesh),
            };
            self.engine = p.engine;
            self.selected = p.selected;
            self.target = p.target;
        }
        // History steps name sets by position; they only stay valid when
        // every set kept its place.
        let identity = assign.len() == old_names.len() && assign.iter().enumerate().all(|(i, a)| *a == Some(i));
        if !identity {
            self.undo.clear();
        }
        self.sets.parked = parked;
        self.sets.active = active;
        self.sets.bake_queue.clear();
        self.undo.set = active;
        let bounds = (self.mesh.bounds_min, self.mesh.bounds_max);
        for (k, (_, range)) in new.into_iter().enumerate() {
            let gpu_mesh = &self.gpu_mesh;
            let e = match self.sets.parked[k].as_mut() {
                Some(p) => &mut p.engine,
                None => &mut self.engine,
            };
            e.set_bounds(bounds);
            e.set_triangles(range, gpu_mesh);
        }
        for d in &mut self.graphs.docs {
            d.dirty = true;
        }
    }

    /// The texture set list: click a set to edit it.
    pub(super) fn texture_set_list(&mut self, ui: &mut egui::Ui) {
        let mut pick = None;
        let mut toggle = None;
        let hidden: Vec<bool> = self.set_engines().iter().map(|e| e.hidden).collect();
        for (k, set) in self.mesh.sets.iter().enumerate() {
            let tris = set.tris.end - set.tris.start;
            ui.horizontal(|ui| {
                let (icon, col) = if hidden[k] { (crate::icons::EYE_SLASH, theme::TEXT_DIM) } else { (crate::icons::EYE, theme::TEXT) };
                let eye = ui
                    .add(egui::Button::new(egui::RichText::new(icon).color(col)).frame(false).min_size(egui::vec2(22.0, 22.0)))
                    .on_hover_text("Show / hide in the views (Ctrl+click: show only this one)");
                if eye.clicked() {
                    toggle = Some((k, ui.input(|i| i.modifiers.command)));
                }
                let row = theme::selectable(k == self.sets.active, &set.name).min_size(egui::vec2(ui.available_width(), 22.0));
                let resp = ui
                    .add(row)
                    .on_hover_text(format!("{} ({tris} triangles). Click to paint this texture set.", set.name));
                if resp.clicked() {
                    pick = Some(k);
                }
            });
        }
        if let Some((k, only)) = toggle {
            let n = self.mesh.sets.len();
            let others_hidden = (0..n).filter(|&j| j != k).all(|j| hidden[j]);
            for j in 0..n {
                let h = if only { !(j == k || others_hidden) } else if j == k { !hidden[k] } else { hidden[j] };
                if let Some(e) = self.set_engine_mut(j) {
                    e.hidden = h;
                }
            }
        }
        if let Some(k) = pick {
            self.switch_set(k);
        }
    }
}

#[cfg(test)]
impl PainterApp {
    /// Each texture set's name, size and layer names, bottom to top (offscreen tests).
    pub(crate) fn test_sets(&self) -> Vec<(String, u32, Vec<String>)> {
        self.mesh
            .sets
            .iter()
            .zip(self.set_engines())
            .map(|(s, e)| (s.name.clone(), e.res, e.layers.iter().map(|l| l.name.clone()).collect()))
            .collect()
    }

    /// Saves the project to `path` and opens it again (offscreen tests).
    pub(crate) fn test_save_and_reopen(&mut self, path: &std::path::Path, ctx: &egui::Context) -> Result<(), String> {
        self.save_project(path)?;
        self.open_project(path, ctx)
    }

    /// A bake is running or queued (offscreen tests).
    pub(crate) fn test_baking(&self) -> bool {
        self.bake_job.is_some() || !self.sets.bake_queue.is_empty()
    }

    /// Turns the path-traced preview on or off (offscreen tests).
    pub(crate) fn test_ray_trace(&mut self, on: bool) {
        self.rt.enabled = on;
    }

    /// The path-traced preview has all its samples (offscreen tests).
    pub(crate) fn test_ray_trace_done(&self) -> bool {
        self.raytracer.converged(&self.rt)
    }

    /// Closes the import report and the autosave prompt (offscreen screenshots).
    pub(crate) fn test_close_dialogs(&mut self) {
        self.show_welcome = false;
        self.recovery = None;
        self.spp.report = None;
    }
}
