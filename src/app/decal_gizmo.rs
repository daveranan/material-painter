//! The decal gizmo: handles in the 3D view that move, turn and scale the
//! selected decal layer.
//!
//! - the center dot slides the decal over the surface (turning it to face
//!   each surface it crosses when "Align to normal" is on);
//! - three arrows move it along its right (red), up (green) and normal
//!   (blue) axes;
//! - the blue ring turns it about its normal; the red and green rings
//!   tilt it (its normal) about its width and height axes;
//! - the diamonds on the normal set the depth: the box reaches that far
//!   in and out (its front and back faces are drawn dashed);
//! - corner squares scale it, edge squares stretch its width or height
//!   (text keeps its proportions).
//!
//! The handles only take clicks that land on them: elsewhere the view
//! paints and orbits as usual.

use super::{PainterApp, decal_hit, theme};
use crate::decal::Decal;
use crate::engine::{Content, Projection};
use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke, pos2};
use glam::{Vec2, Vec3};

/// Grab distance for handles, in screen points.
const GRAB: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Handle {
    Center,
    /// 0 right, 1 up, 2 normal.
    Axis(usize),
    Ring,
    /// Tilts about the right (0) or up (1) axis.
    Tilt(usize),
    /// The depth diamonds on the normal.
    Depth,
    /// Corner k of `Decal::corners`.
    Corner(usize),
    /// 0 right edge, 1 top edge (and their opposites scale the same way).
    Edge(usize),
}

#[derive(Clone, Copy)]
pub(super) struct GizmoDrag {
    layer: u64,
    handle: Handle,
    from: Pos2,
    start: Decal,
    start_rot: f32,
}

/// Where the gizmo's parts are on screen this frame.
struct Layout {
    center: Pos2,
    /// Arrow tips and the 3D axes they stand for.
    tips: [Option<Pos2>; 3],
    axes: [Vec3; 3],
    ring: Vec<Pos2>,
    /// Rings about the right and up axes.
    tilt: [Vec<Pos2>; 2],
    /// The front and back depth diamonds, and the box's front and back faces.
    depth: [Option<Pos2>; 2],
    faces: [Vec<Pos2>; 2],
    corners: Vec<Pos2>,
    /// Right and top edge midpoints, then left and bottom.
    edges: Vec<Pos2>,
    /// The world length of an arrow (constant on screen as the camera moves).
    len: f32,
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let (ab, ap) = (b - a, p - a);
    let t = (ap.dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t).distance(p)
}

impl PainterApp {
    /// The selected layer when it's a decal fill.
    fn gizmo_layer(&self) -> Option<usize> {
        let l = self.engine.layers.get(self.selected)?;
        if self.gizmo_off == Some(l.id) {
            return None;
        }
        match &l.content {
            // Run-offs follow their text: no handles of their own.
            Content::Fill(f) if f.projection == Projection::Decal && l.visible && f.runoff.is_none() => Some(self.selected),
            _ => None,
        }
    }

    fn gizmo_layout(&self, i: usize, rect: Rect) -> Option<Layout> {
        let Content::Fill(f) = &self.engine.layers[i].content else { return None };
        let vp = self.cam.view_proj(rect.width() / rect.height().max(1.0));
        let to_screen = |p: Vec3| {
            let c = vp * p.extend(1.0);
            (c.w > 1e-4).then(|| pos2(rect.min.x + (c.x / c.w + 1.0) * 0.5 * rect.width(), rect.min.y + (1.0 - c.y / c.w) * 0.5 * rect.height()))
        };
        let c3 = Vec3::from(f.decal.center);
        let center = to_screen(c3)?;
        let (r, u, n) = f.decal.axes(f.rotation);
        let len = (self.cam.eye() - c3).length() * 0.09;
        let axes = [r, u, n];
        let tips = axes.map(|a| to_screen(c3 + a * len));
        let circle = |a: Vec3, b: Vec3, radius: f32| -> Vec<Pos2> {
            (0..=48)
                .filter_map(|k| {
                    let t = k as f32 / 48.0 * std::f32::consts::TAU;
                    to_screen(c3 + (a * t.cos() + b * t.sin()) * radius)
                })
                .collect()
        };
        let ring = circle(r, u, len * 1.25);
        let tilt = [circle(u, n, len * 1.1), circle(r, n, len * 1.1)];
        let dd = f.decal.depth;
        let depth = [to_screen(c3 + n * dd), to_screen(c3 - n * dd)];
        let cs = f.decal.corners(f.rotation);
        let face = |off: Vec3| -> Vec<Pos2> {
            let pts: Vec<Pos2> = cs.iter().filter_map(|&p| to_screen(p + off)).collect();
            if pts.len() == 4 { [pts.clone(), vec![pts[0]]].concat() } else { Vec::new() }
        };
        let faces = [face(n * dd), face(-n * dd)];
        let corners: Vec<Pos2> = cs.iter().filter_map(|&p| to_screen(p)).collect();
        let mid = |a: Vec3, b: Vec3| to_screen((a + b) * 0.5);
        let edges = [mid(cs[1], cs[2]), mid(cs[2], cs[3]), mid(cs[3], cs[0]), mid(cs[0], cs[1])].into_iter().flatten().collect();
        Some(Layout { center, tips, axes, ring, tilt, depth, faces, corners, edges, len })
    }

    fn gizmo_pick(&self, l: &Layout, p: Pos2, text: bool) -> Option<Handle> {
        if l.center.distance(p) < GRAB {
            return Some(Handle::Center);
        }
        if l.depth.iter().flatten().any(|d| d.distance(p) < GRAB) {
            return Some(Handle::Depth);
        }
        for (k, c) in l.corners.iter().enumerate() {
            if c.distance(p) < GRAB {
                return Some(Handle::Corner(k));
            }
        }
        if !text && l.edges.len() == 4 {
            for (k, e) in l.edges.iter().enumerate() {
                if e.distance(p) < GRAB {
                    return Some(Handle::Edge(k % 2));
                }
            }
        }
        for (k, t) in l.tips.iter().enumerate() {
            if let Some(t) = t {
                if seg_dist(p, l.center, *t) < GRAB * 0.75 {
                    return Some(Handle::Axis(k));
                }
            }
        }
        if l.ring.windows(2).any(|w| seg_dist(p, w[0], w[1]) < GRAB * 0.75) {
            return Some(Handle::Ring);
        }
        for k in 0..2 {
            if l.tilt[k].windows(2).any(|w| seg_dist(p, w[0], w[1]) < GRAB * 0.75) {
                return Some(Handle::Tilt(k));
            }
        }
        None
    }

    /// The gizmo's input. True when it took the pointer this frame.
    pub(super) fn decal_gizmo_input(&mut self, rect: Rect, hover: Option<Pos2>, pressed: bool, down: bool, shift: bool) -> bool {
        let Some(i) = self.gizmo_layer() else {
            self.decal_drag = None;
            self.decal_hot = None;
            return false;
        };
        let text = self.engine.layers[i].images[0].as_ref().is_some_and(|im| im.text.is_some());
        let Some(l) = self.gizmo_layout(i, rect) else { return false };
        let id = self.engine.layers[i].id;
        // Hover highlight.
        self.decal_hot = if self.decal_drag.is_none() { hover.and_then(|p| self.gizmo_pick(&l, p, text)) } else { self.decal_drag.map(|d| d.handle) };
        if pressed {
            if let (Some(p), Some(h)) = (hover.filter(|p| rect.contains(*p)), self.decal_hot) {
                let Content::Fill(f) = &self.engine.layers[i].content else { return false };
                self.decal_drag = Some(GizmoDrag { layer: id, handle: h, from: p, start: f.decal, start_rot: f.rotation });
                return true;
            }
            return false;
        }
        let Some(drag) = self.decal_drag.filter(|d| d.layer == id) else { return false };
        if !down {
            self.decal_drag = None;
            return true;
        }
        let Some(p) = hover else { return true };
        let align = self.decal_align != shift;
        let size_px = Vec2::new(self.views.view3d.size[0] as f32, self.views.view3d.size[1] as f32);
        let ppp = self.ppp;
        let hit = |p: Pos2| {
            let local = Vec2::new((p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp);
            let (o, d) = self.cam.ray(local, size_px);
            decal_hit(&self.mesh, &self.bvh, o, d)
        };
        let surface = if drag.handle == Handle::Center { hit(p) } else { None };
        let Content::Fill(f) = &mut self.engine.layers[i].content else { return true };
        let s = drag.start;
        let delta = p - drag.from;
        match drag.handle {
            Handle::Center => {
                if let Some((pos, n)) = surface {
                    f.decal.center = pos.to_array();
                    if align {
                        f.decal.normal = n.to_array();
                    }
                }
            }
            Handle::Axis(k) => {
                // Along the arrow on screen, mapped back to its world length.
                if let Some(tip) = l.tips[k] {
                    let v = tip - l.center;
                    let t = delta.dot(v) / v.length_sq().max(1.0) * l.len;
                    f.decal.center = (Vec3::from(s.center) + l.axes[k] * t).to_array();
                }
            }
            Handle::Ring => {
                let a0 = (drag.from - l.center).angle();
                let a1 = (p - l.center).angle();
                // Screen y runs down: a turn that looks clockwise on screen is
                // clockwise about a normal facing the camera.
                let toward = (self.cam.eye() - Vec3::from(s.center)).dot(Vec3::from(s.normal)) >= 0.0;
                let deg = (a1 - a0).to_degrees() * if toward { -1.0 } else { 1.0 };
                f.rotation = ((drag.start_rot + deg + 180.0).rem_euclid(360.0)) - 180.0;
            }
            Handle::Tilt(k) => {
                // Turn the box about its right or up axis (as it was when the
                // drag started), keeping the image's orientation on it.
                let (r0, u0, n0) = s.axes(drag.start_rot);
                let axis = if k == 0 { r0 } else { u0 };
                let a0 = (drag.from - l.center).angle();
                let a1 = (p - l.center).angle();
                let toward = (self.cam.eye() - Vec3::from(s.center)).dot(axis) >= 0.0;
                let rad = (a1 - a0) * if toward { -1.0 } else { 1.0 };
                let q = glam::Quat::from_axis_angle(axis, rad);
                let (n1, r1) = (q * n0, q * r0);
                f.decal.normal = n1.to_array();
                let (rb, ub, _) = Decal { normal: f.decal.normal, ..s }.axes(0.0);
                f.rotation = r1.dot(ub).atan2(r1.dot(rb)).to_degrees();
            }
            Handle::Depth => {
                // Along the normal on screen, as far as the pointer went.
                if let Some(tip) = l.tips[2] {
                    let v = tip - l.center;
                    let along = |q: Pos2| (q - l.center).dot(v) / v.length_sq().max(1.0) * l.len;
                    let t = along(p).abs() - along(drag.from).abs();
                    f.decal.depth = (s.depth + t).clamp(0.002, 10.0);
                }
            }
            Handle::Corner(_) => {
                let k = (p - l.center).length() / (drag.from - l.center).length().max(1.0);
                f.decal.size = s.size.map(|v| (v * k).clamp(1e-3, 10.0));
            }
            Handle::Edge(k) => {
                // The edge's direction on screen, from the center.
                let e = l.edges[k] - l.center;
                let k_scale = ((p - l.center).dot(e) / (drag.from - l.center).dot(e).abs().max(1.0)).max(0.02);
                f.decal.size[k] = (s.size[k] * k_scale).clamp(1e-3, 10.0);
            }
        }
        // A drag: show it as a quick draft (full size once it's let go).
        self.engine.draft = true;
        self.engine.mark_layer_dirty(i);
        true
    }

    /// Snaps the selected decal flat onto the surface under its center.
    pub(super) fn align_decal_to_surface(&mut self, i: usize) {
        let Content::Fill(f) = &self.engine.layers[i].content else { return };
        let (c, n) = (Vec3::from(f.decal.center), Vec3::from(f.decal.normal).normalize_or(Vec3::Y));
        // Look back at the surface from in front of the decal, then from behind.
        let reach = f.decal.depth.max(0.05) * 2.0;
        let found = decal_hit(&self.mesh, &self.bvh, c + n * reach, -n).or_else(|| decal_hit(&self.mesh, &self.bvh, c - n * reach, n));
        if let Some((pos, normal)) = found {
            if let Content::Fill(f) = &mut self.engine.layers[i].content {
                f.decal.center = pos.to_array();
                // The face normal facing the way the decal looked.
                f.decal.normal = (if normal.dot(n) < 0.0 { -normal } else { normal }).to_array();
            }
            self.engine.mark_layer_dirty(i);
        }
    }

    /// The selected decal's box and gizmo in the 3D view.
    pub(super) fn draw_decal(&self, painter: &egui::Painter, rect: Rect) {
        let Some(i) = self.gizmo_layer() else { return };
        let Some(l) = self.gizmo_layout(i, rect) else { return };
        let text = self.engine.layers[i].images[0].as_ref().is_some_and(|im| im.text.is_some());
        let hot = self.decal_hot;
        let lit = |h: Handle, c: Color32| if hot == Some(h) { Color32::from_rgb(255, 230, 80) } else { c };
        let outline = Color32::from_white_alpha(170);
        // The mirrored copy's outline, fainter (it follows; it has no handles).
        if let Content::Fill(f) = &self.engine.layers[i].content
            && f.decal.mirror
        {
            let vp = self.cam.view_proj(rect.width() / rect.height().max(1.0));
            let m = Vec3::new(-1.0, 1.0, 1.0);
            let pts: Vec<Pos2> = f
                .decal
                .corners(f.rotation)
                .iter()
                .filter_map(|&p| {
                    let c = vp * (p * m).extend(1.0);
                    (c.w > 1e-4).then(|| pos2(rect.min.x + (c.x / c.w + 1.0) * 0.5 * rect.width(), rect.min.y + (1.0 - c.y / c.w) * 0.5 * rect.height()))
                })
                .collect();
            if pts.len() == 4 {
                let mut ring = pts.clone();
                ring.push(pts[0]);
                painter.add(egui::Shape::dashed_line(&ring, Stroke::new(1.0, Color32::from_white_alpha(110)), 6.0, 4.0));
            }
        }
        // The box's reach: its front and back faces.
        let dstroke = Stroke::new(if hot == Some(Handle::Depth) { 1.6 } else { 1.0 }, lit(Handle::Depth, Color32::from_rgba_unmultiplied(120, 170, 255, 150)));
        for face in &l.faces {
            if face.len() == 5 {
                painter.add(egui::Shape::dashed_line(face, dstroke, 5.0, 4.0));
            }
        }
        if let [Some(a), Some(b)] = l.depth {
            painter.add(egui::Shape::dashed_line(&[a, b], dstroke, 3.0, 3.0));
        }
        if l.corners.len() == 4 {
            let mut ring = l.corners.clone();
            ring.push(l.corners[0]);
            painter.add(egui::Shape::line(ring, Stroke::new(1.2, outline)));
            // The image's top edge, so a turned decal reads right.
            painter.line_segment([l.corners[3], l.corners[2]], Stroke::new(3.0, outline));
        }
        painter.add(egui::Shape::line(l.ring.clone(), Stroke::new(if hot == Some(Handle::Ring) { 3.0 } else { 1.5 }, lit(Handle::Ring, Color32::from_rgb(90, 170, 255)))));
        let axis_colors = [Color32::from_rgb(235, 80, 70), Color32::from_rgb(110, 210, 90), Color32::from_rgb(80, 140, 255)];
        for k in 0..2 {
            let c = lit(Handle::Tilt(k), axis_colors[k].gamma_multiply(0.8));
            painter.add(egui::Shape::line(l.tilt[k].clone(), Stroke::new(if hot == Some(Handle::Tilt(k)) { 3.0 } else { 1.2 }, c)));
        }
        for (k, tip) in l.tips.iter().enumerate() {
            if let Some(tip) = tip {
                let c = lit(Handle::Axis(k), axis_colors[k]);
                painter.line_segment([l.center, *tip], Stroke::new(if hot == Some(Handle::Axis(k)) { 3.5 } else { 2.0 }, c));
                painter.circle_filled(*tip, 4.0, c);
            }
        }
        let sq = |p: Pos2, h: Handle| {
            painter.rect_filled(Rect::from_center_size(p, egui::vec2(8.0, 8.0)), 1.0, lit(h, Color32::WHITE));
            painter.rect_stroke(Rect::from_center_size(p, egui::vec2(8.0, 8.0)), 1.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
        };
        for (k, c) in l.corners.iter().enumerate() {
            sq(*c, Handle::Corner(k));
        }
        if !text {
            for (k, e) in l.edges.iter().enumerate() {
                sq(*e, Handle::Edge(k % 2));
            }
        }
        for d in l.depth.iter().flatten() {
            let c = lit(Handle::Depth, Color32::from_rgb(120, 170, 255));
            let pts = vec![*d + egui::vec2(0.0, -6.0), *d + egui::vec2(6.0, 0.0), *d + egui::vec2(0.0, 6.0), *d + egui::vec2(-6.0, 0.0)];
            painter.add(egui::Shape::convex_polygon(pts, c, Stroke::new(1.0, Color32::BLACK)));
        }
        painter.circle_filled(l.center, 6.0, lit(Handle::Center, theme::ACCENT));
        painter.circle_stroke(l.center, 6.0, Stroke::new(1.0, Color32::BLACK));
        if let Some(h) = hot {
            let tip = match h {
                Handle::Center if self.decal_align => "Drag: slide over the surface, facing it (Shift: keep the angle)",
                Handle::Center => "Drag: slide over the surface, keeping the angle (Shift: face the surface)",
                Handle::Axis(0) => "Drag: move along the decal's width",
                Handle::Axis(1) => "Drag: move along the decal's height",
                Handle::Axis(_) => "Drag: move along the normal (in and out of the surface)",
                Handle::Ring => "Drag: turn about the normal",
                Handle::Tilt(0) => "Drag: tilt about the width axis (slant it)",
                Handle::Tilt(_) => "Drag: tilt about the height axis (slant it)",
                Handle::Depth => "Drag: depth, how far the box reaches in and out of the surface",
                Handle::Corner(_) => "Drag: scale",
                Handle::Edge(0) => "Drag: stretch the width",
                Handle::Edge(_) => "Drag: stretch the height",
            };
            painter.text(l.center + egui::vec2(12.0, -14.0), egui::Align2::LEFT_BOTTOM, tip, egui::FontId::proportional(12.0), Color32::WHITE);
        }
    }
}
