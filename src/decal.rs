//! Decals: a box placed on the surface that a fill layer shows inside.
//!
//! The box sits at `center`, faces along `normal` and spans `size` across
//! the surface and `depth` into and out of it, so it catches the surface it
//! was placed on and nothing behind. Without an image the whole box shows
//! (bands, stripes, accent blocks); with one, the image's alpha cuts out
//! the shape and its color tints the layer's (text, insignia).
//!
//! Text decals keep their [`TextSpec`] with the image, so editing the text
//! renders the image again.

use glam::Vec3;

/// Where a decal sits. Positions are in the mesh's normalized space (the
/// space of the baked position map and the viewport).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Decal {
    pub center: [f32; 3],
    /// The direction the decal projects along: the surface normal where it was placed.
    pub normal: [f32; 3],
    /// Width and height across the surface.
    pub size: [f32; 2],
    /// Reach into and out of the surface, each way.
    pub depth: f32,
    /// Faces turned further than this many degrees from `normal` don't take
    /// the decal (180 wraps it around everything in the box).
    pub facing: f32,
    /// Edge softness, as a fraction of the size.
    pub soft: f32,
    /// Also on the other side of the mesh, mirrored across x = 0 (text stays
    /// readable there).
    pub mirror: bool,
}

impl Default for Decal {
    fn default() -> Self {
        Decal { center: [0.0; 3], normal: [0.0, 1.0, 0.0], size: [0.4, 0.1], depth: 0.1, facing: 80.0, soft: 0.01, mirror: false }
    }
}

impl Decal {
    /// Right, up and projection axes. Image up follows world up on walls;
    /// on faces looking up or down it runs along -Z. `rotation` (degrees)
    /// spins the box about its normal.
    pub fn axes(&self, rotation: f32) -> (Vec3, Vec3, Vec3) {
        let n = Vec3::from(self.normal).try_normalize().unwrap_or(Vec3::Y);
        let hint = if n.y.abs() > 0.95 { Vec3::NEG_Z } else { Vec3::Y };
        let right = hint.cross(n).normalize();
        let up = n.cross(right);
        let (s, c) = rotation.to_radians().sin_cos();
        (right * c + up * s, up * c - right * s, n)
    }

    /// The four corners on the surface plane, counter-clockwise from bottom left.
    pub fn corners(&self, rotation: f32) -> [Vec3; 4] {
        let (r, u, _) = self.axes(rotation);
        let c = Vec3::from(self.center);
        let (hw, hh) = (self.size[0] * 0.5, self.size[1] * 0.5);
        [c - r * hw - u * hh, c + r * hw - u * hh, c + r * hw + u * hh, c - r * hw + u * hh]
    }

    /// What the composite shader reads (`LayerU.dc0..dc3`): the center and
    /// depth, the right and up axes over the size (so a dot product gives
    /// -0.5..0.5 across the box), softness, the facing limit's cosine and the
    /// normal.
    pub fn uniform(&self, rotation: f32) -> [[f32; 4]; 4] {
        let (r, u, n) = self.axes(rotation);
        let r = r / self.size[0].max(1e-5);
        let u = u / self.size[1].max(1e-5);
        let c = self.center;
        [
            [c[0], c[1], c[2], self.depth.max(1e-5)],
            [r.x, r.y, r.z, self.soft.clamp(0.0, 0.5)],
            [u.x, u.y, u.z, self.facing.clamp(0.0, 180.0).to_radians().cos()],
            [n.x, n.y, n.z, if self.mirror { 1.0 } else { 0.0 }],
        ]
    }

    /// Whether the box takes the surface point `p` (normal `n`), on either
    /// side when mirrored.
    pub fn covers(&self, rotation: f32, p: Vec3, n: Vec3) -> bool {
        let one = |p: Vec3, n: Vec3| {
            let (r, u, dn) = self.axes(rotation);
            let q = p - Vec3::from(self.center);
            let (x, y, z) = (q.dot(r) / self.size[0].max(1e-5), q.dot(u) / self.size[1].max(1e-5), q.dot(dn) / self.depth.max(1e-5));
            let facing = self.facing >= 179.9 || n.dot(dn) >= self.facing.to_radians().cos();
            x.abs() <= 0.5 && y.abs() <= 0.5 && z.abs() <= 1.0 && facing
        };
        let m = Vec3::new(-1.0, 1.0, 1.0);
        one(p, n) || (self.mirror && one(p * m, n * m))
    }
}

/// The text a decal image was rendered from.
#[derive(Clone, PartialEq, Debug)]
pub struct TextSpec {
    pub text: String,
    /// A font file, or the name of an installed one ("bahnschrift", "arialbd").
    pub font: String,
    /// Weight for variable fonts (100 thin .. 900 black); others ignore it.
    pub weight: f32,
    /// Extra space between letters, in ems.
    pub spacing: f32,
}

impl Default for TextSpec {
    fn default() -> Self {
        TextSpec { text: "TEXT".into(), font: "bahnschrift".into(), weight: 600.0, spacing: 0.04 }
    }
}

/// Fonts offered in the font picker: names of installed fonts that suit markings.
pub const FONT_CHOICES: [&str; 6] = ["bahnschrift", "arialbd", "impact", "consolab", "segoeuib", "verdanab"];

/// The font file for a name or path.
pub fn font_path(font: &str) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(font);
    if p.is_file() {
        return Some(p);
    }
    let dirs: Vec<std::path::PathBuf> = [
        std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts")),
        std::env::var_os("LOCALAPPDATA").map(|l| std::path::PathBuf::from(l).join("Microsoft/Windows/Fonts")),
        Some("/usr/share/fonts/truetype".into()),
        Some("/Library/Fonts".into()),
    ]
    .into_iter()
    .flatten()
    .collect();
    for d in &dirs {
        for ext in ["ttf", "otf", "TTF", "OTF"] {
            let f = d.join(format!("{font}.{ext}"));
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

/// Renders text as white with coverage in alpha, with a margin around it,
/// `px` pixels per em. Lines split at '\n'.
pub fn render_text(spec: &TextSpec, px: f32) -> Result<image::RgbaImage, String> {
    use ab_glyph::{Font, FontVec, PxScale, ScaleFont, VariableFont};
    let path = font_path(&spec.font)
        .or_else(|| font_path("bahnschrift"))
        .or_else(|| font_path("arialbd"))
        .ok_or_else(|| format!("font '{}' not found", spec.font))?;
    let bytes = std::fs::read(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let mut font = FontVec::try_from_vec(bytes).map_err(|_| format!("{} isn't a font", path.display()))?;
    font.set_variation(b"wght", spec.weight);
    let scale = PxScale::from(px);
    let sf = font.as_scaled(scale);
    let lines: Vec<&str> = if spec.text.is_empty() { vec![" "] } else { spec.text.split('\n').collect() };
    let line_h = sf.ascent() - sf.descent() + sf.line_gap();
    let spacing = spec.spacing * px;
    // Lay out each line, then measure.
    let mut placed = Vec::new();
    let mut width = 0f32;
    for (k, line) in lines.iter().enumerate() {
        let mut x = 0.0;
        let mut prev = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            placed.push(id.with_scale_and_position(scale, ab_glyph::point(x, sf.ascent() + k as f32 * line_h)));
            x += sf.h_advance(id) + spacing;
            prev = Some(id);
        }
        width = width.max(x - spacing);
    }
    let height = line_h * lines.len() as f32 - sf.line_gap();
    let pad = (px * 0.12).ceil();
    let (w, h) = ((width + 2.0 * pad).ceil().max(1.0) as u32, (height + 2.0 * pad).ceil().max(1.0) as u32);
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 0]));
    for g in placed {
        if let Some(o) = font.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|x, y, c| {
                let (px, py) = (x as i64 + b.min.x as i64 + pad as i64, y as i64 + b.min.y as i64 + pad as i64);
                if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                    let a = &mut img.get_pixel_mut(px as u32, py as u32).0[3];
                    *a = (*a).max((c.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            });
        }
    }
    Ok(img)
}

/// Paint running down from a text decal: streaks that start under the
/// letters and fade out, `length` letter-image heights long.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Runoff {
    /// The text decal layer it runs from (a layer id).
    pub source: u64,
    /// Below the letters, in heights of the text image.
    pub length: f32,
    pub seed: f32,
}

/// The run-off image for text: as wide as the text image, `1 + length`
/// times as tall, the letters' place at the top left empty. White, with the
/// streaks in alpha.
pub fn render_runoff(spec: &TextSpec, px: f32, length: f32, seed: f32) -> Result<image::RgbaImage, String> {
    let letters = render_text(spec, px)?;
    let (w, h) = (letters.width() as usize, letters.height() as usize);
    let rows = ((h as f32) * (1.0 + length.max(0.0))).ceil().max(h as f32 + 1.0) as usize;
    let canvas = |x: usize, y: usize| if y < h { letters.get_pixel(x as u32, y as u32).0[3] as f32 / 255.0 } else { 0.0 };
    // Per column: how far a run goes and how much paint it carries, a few
    // long runs among many short ones, smooth across neighbouring columns.
    let mut state = (seed.to_bits() as u64 ^ 0x9e37_79b9_7f4a_7c15).max(1);
    let mut rnd = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    let smooth = |v: &mut Vec<f32>, r: usize| {
        let src = v.clone();
        for (i, out) in v.iter_mut().enumerate() {
            let (a, b) = (i.saturating_sub(r), (i + r + 1).min(src.len()));
            *out = src[a..b].iter().sum::<f32>() / (b - a) as f32;
        }
    };
    let mut run: Vec<f32> = (0..w).map(|_| h as f32 * length * (0.1 + 1.4 * rnd().powi(2)) + 4.0).collect();
    let mut amount: Vec<f32> = (0..w).map(|_| rnd().powf(2.5)).collect();
    smooth(&mut run, 2);
    smooth(&mut amount, 1);
    let mut streak = vec![0f32; w * rows];
    for x in 0..w {
        let decay = (-1.0 / run[x].max(1.0)).exp();
        let mut s = 0f32;
        for y in 0..rows {
            s = (canvas(x, y) * amount[x]).max(s * decay);
            // Not over the letters themselves (nor right around them).
            let near = (y.saturating_sub(1)..(y + 2).min(rows)).any(|yy| (x.saturating_sub(1)..(x + 2).min(w)).any(|xx| canvas(xx, yy) > 0.3));
            streak[y * w + x] = if near { 0.0 } else { s };
        }
    }
    // Soften: a little across, more along the runs.
    let blur = |v: &mut Vec<f32>, rx: usize, ry: usize| {
        let src = v.clone();
        for y in 0..rows {
            for x in 0..w {
                let (x0, x1, y0, y1) = (x.saturating_sub(rx), (x + rx + 1).min(w), y.saturating_sub(ry), (y + ry + 1).min(rows));
                let mut sum = 0.0;
                for yy in y0..y1 {
                    sum += src[yy * w + x0..yy * w + x1].iter().sum::<f32>();
                }
                v[y * w + x] = sum / ((x1 - x0) * (y1 - y0)) as f32;
            }
        }
    };
    blur(&mut streak, 1, 2);
    let mut sorted: Vec<f32> = streak.iter().copied().filter(|v| *v > 0.0).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let top = sorted.get(sorted.len().saturating_sub(1) * 995 / 1000).copied().unwrap_or(1.0).max(1e-6);
    let mut img = image::RgbaImage::from_pixel(w as u32, rows as u32, image::Rgba([255, 255, 255, 0]));
    for (i, v) in streak.iter().enumerate() {
        img.get_pixel_mut((i % w) as u32, (i / w) as u32).0[3] = ((v / top).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_are_orthonormal_and_follow_rotation() {
        for normal in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.3, 0.2, -0.9]] {
            let d = Decal { normal, ..Decal::default() };
            for rot in [0.0, 37.0, 90.0] {
                let (r, u, n) = d.axes(rot);
                assert!((r.length() - 1.0).abs() < 1e-4 && (u.length() - 1.0).abs() < 1e-4);
                assert!(r.dot(u).abs() < 1e-4 && r.dot(n).abs() < 1e-4 && u.dot(n).abs() < 1e-4);
                // Right-handed: right x up = normal.
                assert!((r.cross(u) - n).length() < 1e-4, "{normal:?} {rot}");
            }
        }
        // On a wall, image up is world up.
        let (_, u, _) = Decal { normal: [1.0, 0.0, 0.0], ..Decal::default() }.axes(0.0);
        assert!((u - Vec3::Y).length() < 1e-4);
    }

    #[test]
    fn uniform_maps_the_box_to_unit_coordinates() {
        let d = Decal { center: [0.1, 0.2, 0.3], normal: [0.0, 0.0, 1.0], size: [0.5, 0.25], ..Decal::default() };
        assert!(d.covers(0.0, Vec3::new(0.1, 0.2, 0.3), Vec3::Z));
        assert!(!d.covers(0.0, Vec3::new(-0.3, 0.2, 0.3), Vec3::Z), "no mirror: not on the other side");
        let m = Decal { mirror: true, ..d };
        assert!(m.covers(0.0, Vec3::new(-0.3, 0.2, 0.3), Vec3::Z), "mirrored across x = 0");
        let [c, r, u, _] = d.uniform(0.0);
        let corner = d.corners(0.0)[2];
        let local = corner - Vec3::new(c[0], c[1], c[2]);
        assert!((local.dot(Vec3::new(r[0], r[1], r[2])) - 0.5).abs() < 1e-4);
        assert!((local.dot(Vec3::new(u[0], u[1], u[2])) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn runoff_runs_below_the_letters_only() {
        if font_path("bahnschrift").or_else(|| font_path("arialbd")).is_none() {
            return;
        }
        let spec = TextSpec { text: "C-3".into(), ..TextSpec::default() };
        let letters = render_text(&spec, 64.0).unwrap();
        let run = render_runoff(&spec, 64.0, 1.0, 3.0).unwrap();
        assert_eq!(run.width(), letters.width());
        assert!(run.height() >= letters.height() * 2 - 1, "{} vs {}", run.height(), letters.height());
        let alpha = |img: &image::RgbaImage, x: u32, y: u32| img.get_pixel(x, y).0[3];
        // Nothing over the letters, something below them.
        let over = (0..letters.height()).flat_map(|y| (0..letters.width()).map(move |x| (x, y))).filter(|&(x, y)| alpha(&letters, x, y) > 200 && alpha(&run, x, y) > 64).count();
        let below = (letters.height()..run.height()).flat_map(|y| (0..run.width()).map(move |x| (x, y))).filter(|&(x, y)| alpha(&run, x, y) > 64).count();
        assert_eq!(over, 0, "streaks over the letters");
        assert!(below > 100, "too few streaks below: {below}");
    }

    #[test]
    fn text_renders_when_a_font_is_installed() {
        if font_path("bahnschrift").or_else(|| font_path("arialbd")).is_none() {
            return;
        }
        let img = render_text(&TextSpec { text: "UCF C-3".into(), ..TextSpec::default() }, 64.0).unwrap();
        assert!(img.width() > img.height() * 3, "{}x{}", img.width(), img.height());
        let covered = img.pixels().filter(|p| p.0[3] > 128).count();
        assert!(covered > 500 && covered < (img.width() * img.height()) as usize / 2, "{covered}");
    }
}
