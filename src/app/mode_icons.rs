//! The display-mode switch on the 3D view's tab bar: one small sphere per view
//! mode, drawn on the CPU once so each icon shows what that channel looks
//! like (lit material, flat base color, grayscale masks, normal colors...).

use super::{PainterApp, theme};
use crate::engine::MAPS;
use crate::viewport::ViewMode;
use eframe::egui;
use egui::{Color32, Sense, Stroke, vec2};
use glam::Vec3;
use std::collections::HashMap;

/// Icon image size in pixels (drawn at half that, so it stays crisp on HiDPI).
const PX: usize = 44;
/// On-screen icon size.
const SIZE: f32 = 22.0;

/// Sphere icons, one per view mode; custom maps share one.
#[derive(Default)]
pub(super) struct ModeIcons {
    tex: HashMap<u32, egui::TextureHandle>,
}

impl ModeIcons {
    fn get(&mut self, ctx: &egui::Context, mode: ViewMode) -> egui::TextureId {
        let key = mode.index();
        self.tex
            .entry(key)
            .or_insert_with(|| ctx.load_texture(format!("mode icon {key}"), sphere_icon(mode), egui::TextureOptions::LINEAR))
            .id()
    }
}

impl PainterApp {
    /// The row of mode spheres. Called from a right-to-left tab bar, so the
    /// groups are laid out last to first.
    pub(super) fn view_mode_strip(&mut self, ui: &mut egui::Ui) {
        let customs: Vec<(u32, String)> = self.bake_settings.custom.iter().map(|c| (c.id, c.name.clone())).collect();
        let mut groups: Vec<Vec<(ViewMode, String, &'static str)>> = vec![
            ViewMode::CHANNELS.iter().map(|&m| (m, m.name().to_string(), channel_desc(m))).collect(),
            (0..MAPS.len()).map(|i| (ViewMode::Map(i), MAPS[i].name.to_string(), MAPS[i].desc)).collect(),
            [ViewMode::WorldNormal, ViewMode::Position, ViewMode::MissedRays]
                .iter()
                .map(|&m| (m, m.name().to_string(), channel_desc(m)))
                .collect(),
        ];
        if !customs.is_empty() {
            groups.push(customs.into_iter().map(|(id, name)| (ViewMode::Custom(id), name, "Custom baked map.")).collect());
        }
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.add_space(4.0);
        for (gi, group) in groups.iter().enumerate().rev() {
            for (mode, name, desc) in group.iter().rev() {
                self.mode_sphere(ui, *mode, name, desc);
            }
            if gi > 0 {
                ui.add_space(3.0);
                ui.separator();
                ui.add_space(3.0);
            }
        }
    }

    fn mode_sphere(&mut self, ui: &mut egui::Ui, mode: ViewMode, name: &str, desc: &str) {
        let tex = self.mode_icons.get(ui.ctx(), mode);
        let (rect, resp) = ui.allocate_exact_size(vec2(SIZE + 2.0, SIZE + 2.0), Sense::click());
        let on = self.view.mode == mode;
        let p = ui.painter();
        if on {
            p.rect_filled(rect, 0.0, theme::ACCENT_DIM);
            p.rect_stroke(rect, 0.0, Stroke::new(1.0, theme::ACCENT), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect_filled(rect, 0.0, Color32::from_white_alpha(18));
        }
        let tint = if on || resp.hovered() { Color32::WHITE } else { Color32::from_gray(205) };
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        p.image(tex, egui::Rect::from_center_size(rect.center(), vec2(SIZE, SIZE)), uv, tint);
        let resp = resp.on_hover_ui(|ui| {
            ui.label(egui::RichText::new(name).strong());
            ui.label(egui::RichText::new(desc).color(theme::TEXT_DIM));
        });
        if resp.clicked() {
            self.view.mode = mode;
        }
    }
}

fn channel_desc(m: ViewMode) -> &'static str {
    match m {
        ViewMode::Material => "The lit material (M).",
        ViewMode::BaseColor => "Albedo color, without lighting.",
        ViewMode::Roughness => "Dark = glossy, white = matte.",
        ViewMode::Metallic => "White = metal, black = non-metal.",
        ViewMode::Height => "Painted height: white = raised.",
        ViewMode::Normal => "The final tangent-space normal: mesh normal map plus painted height.",
        ViewMode::Emission => "Light the surface gives off.",
        ViewMode::WorldNormal => "Mesh normals in world space (X red, Y green, Z blue).",
        ViewMode::Position => "World position as color.",
        ViewMode::MissedRays => "Where baking rays missed the high poly: red marks need a bigger cage.",
        _ => "",
    }
}

// --- Icon drawing ----------------------------------------------------------

/// Light direction for the lit icons: upper left, toward the viewer.
const LIGHT: Vec3 = Vec3::new(-0.55, 0.65, 0.52);

fn hash(p: [i32; 3]) -> f32 {
    let mut h = (p[0] as u32).wrapping_mul(0x8da6_b343) ^ (p[1] as u32).wrapping_mul(0xd816_3841) ^ (p[2] as u32).wrapping_mul(0xcb1a_b31f);
    h ^= h >> 13;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 65535.0
}

fn noise(p: Vec3) -> f32 {
    let i = p.floor();
    let f = p - i;
    let s = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let c = |x: i32, y: i32, z: i32| hash([i.x as i32 + x, i.y as i32 + y, i.z as i32 + z]);
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), s.x);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), s.x);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), s.x);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), s.x);
    lerp(lerp(x00, x10, s.y), lerp(x01, x11, s.y), s.z)
}

fn fbm(p: Vec3) -> f32 {
    noise(p) * 0.55 + noise(p * 2.1 + 7.3) * 0.3 + noise(p * 4.3 + 1.7) * 0.15
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Panels on the sphere: 8 slices around, 4 bands top to bottom, split by
/// grooves. Returns (panel id, distance to the nearest groove, height in the
/// panel from 0 at its bottom to 1 at its top).
fn panels(n: Vec3) -> (i32, f32, f32) {
    use std::f32::consts::PI;
    let theta = n.x.atan2(n.z);
    let phi = n.y.clamp(-1.0, 1.0).asin();
    let a = (theta / (2.0 * PI) + 0.5) * 8.0;
    let b = (phi / PI + 0.5) * 4.0;
    let da = (a - a.round()).abs() * (2.0 * PI / 8.0) * phi.cos();
    let db = (b - b.round()).abs() * (PI / 4.0);
    let id = a.floor() as i32 + 8 * b.floor() as i32;
    (id, da.min(db), b.fract())
}

/// Groove profile: 0 in the groove, 1 on the panel face.
fn groove_height(n: Vec3) -> f32 {
    smooth(0.02, 0.11, panels(n).1)
}

fn hue(h: f32) -> Vec3 {
    let k = |o: f32| (((h * 6.0 + o) % 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0);
    Vec3::new(k(0.0), k(4.0), k(2.0))
}

fn albedo(n: Vec3) -> Vec3 {
    let rust = fbm(n * 3.5);
    let base = Vec3::new(0.42, 0.40, 0.38);
    let orange = Vec3::new(0.45, 0.18, 0.06);
    base.lerp(orange, smooth(0.45, 0.62, rust)) * (0.85 + 0.3 * fbm(n * 12.0))
}

/// Tangent-ish normal colors from a height function, by finite differences
/// across the sphere surface.
fn normal_color(n: Vec3, h: impl Fn(Vec3) -> f32, strength: f32) -> Vec3 {
    let t = Vec3::Y.cross(n).normalize_or(Vec3::X);
    let b = n.cross(t);
    let e = 0.01;
    let dx = (h((n + t * e).normalize()) - h((n - t * e).normalize())) / (2.0 * e);
    let dy = (h((n + b * e).normalize()) - h((n - b * e).normalize())) / (2.0 * e);
    let nt = Vec3::new(-dx * strength, dy * strength, 1.0).normalize();
    nt * 0.5 + 0.5
}

/// The color of one point on the sphere for a view mode, in display (sRGB)
/// values 0..1. `n` is the surface normal, `u`, `v` the screen position.
fn shade(mode: ViewMode, n: Vec3, u: f32, v: f32) -> Vec3 {
    let gray = |x: f32| Vec3::splat(x.clamp(0.0, 1.0));
    let bumps = |p: Vec3| fbm(p * 3.0);
    let c = match mode {
        ViewMode::Material => {
            let l = LIGHT.normalize();
            let a = albedo(n);
            let rough = fbm(n * 3.5);
            let metal = smooth(0.45, 0.62, rough) < 0.5;
            let diffuse = n.dot(l).max(0.0);
            let h = (l + Vec3::Z).normalize();
            let shine = if metal { 60.0 } else { 18.0 };
            let spec = n.dot(h).max(0.0).powf(shine) * if metal { 1.1 } else { 0.25 };
            let spec_col = if metal { a * 1.6 + 0.15 } else { Vec3::ONE };
            let ambient = 0.10 + 0.12 * (n.y * 0.5 + 0.5);
            let lit = a * (diffuse * if metal { 0.45 } else { 1.0 } + ambient) + spec_col * spec;
            let lin = lit * groove_height(n).mul_add(0.35, 0.65);
            Vec3::new(lin.x.powf(1.0 / 2.2), lin.y.powf(1.0 / 2.2), lin.z.powf(1.0 / 2.2))
        }
        ViewMode::BaseColor => {
            let a = albedo(n);
            Vec3::new(a.x.powf(1.0 / 2.2), a.y.powf(1.0 / 2.2), a.z.powf(1.0 / 2.2))
        }
        ViewMode::Roughness => gray(0.25 + 0.6 * smooth(0.3, 0.75, fbm(n * 5.0)) + 0.1 * (noise(n * 30.0) - 0.5)),
        ViewMode::Metallic => gray(1.0 - smooth(0.49, 0.53, fbm(n * 3.5))),
        ViewMode::Height => gray(0.15 + 0.85 * smooth(0.25, 0.75, bumps(n))),
        ViewMode::Normal => normal_color(n, bumps, 0.45),
        ViewMode::Emission => {
            let d = (fbm(n * 2.2) - 0.5).abs();
            let crack = 1.0 - smooth(0.0, 0.05, d);
            let glow = (1.0 - smooth(0.0, 0.16, d)) * 0.45;
            Vec3::new(0.08, 0.08, 0.09).lerp(Vec3::new(1.0, 0.6, 0.2), (crack + glow).min(1.0))
        }
        ViewMode::Map(i) => {
            let (id, d, grad) = panels(n);
            let g = groove_height(n);
            match MAPS[i].name {
                "Ambient Occlusion" => gray((0.35 + 0.65 * smooth(-0.9, 0.6, n.y)) * g.mul_add(0.45, 0.55)),
                "Curvature" => {
                    let ridge = (1.0 - smooth(0.0, 0.04, (d - 0.12).abs())) * 0.45;
                    gray(0.5 + ridge - (1.0 - g) * 0.4)
                }
                "Cavity" => gray(0.25 + 0.75 * g),
                "Thickness" => gray(0.12 + 0.88 * n.z.powf(0.7)),
                "Sky Exposure" => gray(smooth(-0.55, 0.9, n.y)),
                "Edge Distance" => gray(smooth(0.0, 0.32, d)),
                "Crease Distance" => gray(smooth(0.0, 0.7, n.y.abs())),
                "Part Random" => gray(0.15 + 0.8 * hash([id, 3, 7])),
                "Part Gradient" => gray(grad),
                "Bent Normal" => (n + Vec3::new(0.0, 0.6, 0.0)).normalize() * 0.5 + 0.5,
                "Normal" => normal_color(n, groove_height, 0.35),
                "Height" => gray(0.2 + 0.8 * g),
                "ID" => hue(hash([id, 11, 5])) * 0.85 + 0.1,
                _ => gray(fbm(n * 4.0)),
            }
        }
        ViewMode::WorldNormal => n * 0.5 + 0.5,
        ViewMode::Position => Vec3::new(u * 0.5 + 0.5, v * 0.5 + 0.5, n.z),
        ViewMode::MissedRays => {
            let miss = hash([(u * 9.0) as i32, (v * 9.0) as i32, 1]) > 0.85 && fbm(n * 2.5) > 0.5;
            if miss { Vec3::new(0.95, 0.15, 0.12) } else { gray(0.22) }
        }
        ViewMode::Custom(_) => gray(0.2 + 0.7 * smooth(0.25, 0.75, fbm(n * 4.0 + 3.0))),
    };
    if mode == ViewMode::Material {
        c
    } else {
        // A touch of rim darkening so flat channels still read as a ball.
        c * (0.78 + 0.22 * n.z)
    }
}

/// One icon: a sphere filling the image, antialiased by 3x3 supersampling.
fn sphere_icon(mode: ViewMode) -> egui::ColorImage {
    const SS: usize = 3;
    let mut pixels = Vec::with_capacity(PX * PX);
    for y in 0..PX {
        for x in 0..PX {
            let mut sum = Vec3::ZERO;
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = ((x * SS + sx) as f32 + 0.5) / (PX * SS) as f32 * 2.0 - 1.0;
                    let v = 1.0 - ((y * SS + sy) as f32 + 0.5) / (PX * SS) as f32 * 2.0;
                    let (u, v) = (u * 1.06, v * 1.06);
                    let r2 = u * u + v * v;
                    if r2 > 1.0 {
                        continue;
                    }
                    let n = Vec3::new(u, v, (1.0 - r2).sqrt());
                    sum += shade(mode, n, u, v);
                    hits += 1;
                }
            }
            let c = if hits > 0 { sum / hits as f32 } else { Vec3::ZERO };
            let to8 = |f: f32| (f.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let a = (hits * 255 / (SS * SS)) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(to8(c.x), to8(c.y), to8(c.z), a));
        }
    }
    egui::ColorImage::new([PX, PX], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_icon_has_a_disc_and_transparent_corners() {
        for mode in ViewMode::all().into_iter().chain([ViewMode::Custom(1)]) {
            let img = sphere_icon(mode);
            assert_eq!(img.pixels[0].a(), 0, "{mode:?} corner");
            assert_eq!(img.pixels[PX * PX / 2 + PX / 2].a(), 255, "{mode:?} center");
        }
    }
}
