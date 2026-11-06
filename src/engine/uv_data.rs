//! The UV-bound part of a layer stack: paint channels, paint masks and map
//! fixes. They belong to one mesh's UV layout; a mesh set keeps one copy per
//! member and swaps it in when that member is shown, while everything else
//! about the stack (fills, generators, folders, settings) stays shared.

use super::{Content, Engine, MAP_COUNT};
use crate::gpu::Tex;
use std::collections::HashMap;

/// One layer's UV-bound textures.
#[derive(Default)]
pub struct LayerUv {
    /// A paint layer's channels (`None` for other layers).
    pub channels: Option<[Option<Tex>; 4]>,
    /// The mask's painting (not for graph masks, which render again).
    pub mask: Option<Tex>,
}

/// A stack's UV-bound textures, by layer id.
#[derive(Default)]
pub struct UvData {
    pub layers: HashMap<u64, LayerUv>,
    pub map_fix: Option<[Option<Tex>; MAP_COUNT]>,
}

impl UvData {
    pub fn vram_bytes(&self) -> u64 {
        let layer = |l: &LayerUv| {
            l.channels.iter().flatten().flatten().map(|t| t.bytes()).sum::<u64>() + l.mask.as_ref().map_or(0, |t| t.bytes())
        };
        self.layers.values().map(layer).sum::<u64>() + self.map_fix.iter().flatten().flatten().map(|t| t.bytes()).sum::<u64>()
    }
}

impl Engine {
    /// Takes the stack's UV-bound textures out and puts `incoming` in. A
    /// layer `incoming` has nothing for starts blank: no paint, and a paint
    /// mask that is white over a smart mask (it multiplies) and black
    /// otherwise (shows nothing until painted). Textures of another size are
    /// resampled to the texture set's.
    pub fn swap_uv_data(&mut self, mut incoming: UvData) -> UvData {
        let res = self.res;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut fit = |e: &Engine, t: Tex| if t.width == res && t.height == res { t } else { e.resample(&mut encoder, &t, res) };
        let mut out = UvData::default();
        let mut fresh_masks = Vec::new();
        let mut layers = std::mem::take(&mut self.layers);
        for (li, layer) in layers.iter_mut().enumerate() {
            Self::release_thumb(&self.rs, layer);
            let mut came = incoming.layers.remove(&layer.id).unwrap_or_default();
            let mut taken = LayerUv::default();
            if let Content::Paint(ch) = &mut layer.content {
                let new = came.channels.take().unwrap_or_default().map(|t| t.map(|t| fit(self, t)));
                taken.channels = Some(std::mem::replace(ch, new));
            }
            if let Some(m) = layer.mask.as_mut().filter(|m| m.graph.is_none()) {
                if let Some(old) = m.paint.take() {
                    taken.mask = Some(old);
                    match came.mask.take() {
                        Some(t) => m.paint = Some(fit(self, t)),
                        None => fresh_masks.push((li, m.generator.is_some() || !m.layers.is_empty())),
                    }
                }
            }
            out.layers.insert(layer.id, taken);
        }
        out.map_fix = Some(std::mem::replace(&mut self.map_fix, Default::default()));
        if let Some(fix) = incoming.map_fix.take() {
            self.map_fix = fix.map(|t| t.map(|t| fit(self, t)));
        }
        drop(fit);
        self.queue.submit([encoder.finish()]);
        for (li, white) in fresh_masks {
            let tex = self.new_mask_texture(white);
            if let Some(m) = layers[li].mask.as_mut() {
                m.paint = Some(tex);
            }
        }
        for layer in &mut layers {
            Self::attach_thumb(&self.rs, layer);
        }
        self.layers = layers;
        self.snap = [None, None, None, None];
        self.snap_mask = None;
        self.maps_dirty = true;
        self.invalidate_thumbs();
        self.mark_dirty();
        out
    }
}
