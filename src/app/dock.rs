//! Docking: every panel of a workspace is a tab that can be dragged into
//! another tab group or split off beside, above or below the others
//! (egui_tiles). Layouts are saved per workspace and can be reset.

use super::{PainterApp, ViewKind, Workspace, theme};
use eframe::egui;
use egui::{Color32, Stroke};
use egui_tiles::{Behavior, Container, SimplificationOptions, TabState, Tile, TileId, Tiles, Tree, UiResponse};

/// A dockable panel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub(super) enum Pane {
    View3d,
    View2d,
    Layers,
    Properties,
    MeshMaps,
    TextureSet,
    /// Meshes sharing the layer stack (`mesh_set`).
    MeshSet,
    Preview,
    Shelf,
    /// Material Designer: the material on the mesh.
    DesignView,
    /// Material Designer: the node canvas (or the start page).
    Graph,
    /// Material Designer: the selected node's settings.
    NodeSettings,
    ShaderList,
    /// Shader Maker: the code (or the start page).
    ShaderCode,
    ShaderPreview,
}

impl Pane {
    fn title(self) -> String {
        use crate::icons as i;
        let (icon, name) = match self {
            Pane::View3d => (i::CUBE, "3D View"),
            Pane::View2d => (i::SQUARE, "2D View"),
            Pane::Layers => (i::STACK, "Layers"),
            Pane::Properties => (i::GEAR, "Properties"),
            Pane::MeshMaps => (i::FIRE, "Mesh Maps"),
            Pane::TextureSet => (i::GRID_FOUR, "Texture Set"),
            Pane::MeshSet => (i::CUBE_TRANSPARENT, "Mesh Set"),
            Pane::Preview => (i::APERTURE, "Preview"),
            Pane::Shelf => (i::IMAGES, "Shelf"),
            Pane::DesignView => (i::CUBE, "3D View"),
            Pane::Graph => (i::SPARKLE, "Graph"),
            Pane::NodeSettings => (i::GEAR, "Node"),
            Pane::ShaderList => (i::FOLDER, "Shaders"),
            Pane::ShaderCode => (i::MAGIC_WAND, "Code"),
            Pane::ShaderPreview => (i::APERTURE, "Preview"),
        };
        format!("{icon}  {name}")
    }

    /// Viewports fill their tab edge to edge; panels get padding.
    fn padded(self) -> bool {
        !matches!(self, Pane::View3d | Pane::View2d | Pane::DesignView | Pane::Graph)
    }
}

/// Every pane a workspace's layout must hold.
fn panes(ws: Workspace) -> &'static [Pane] {
    match ws {
        Workspace::Paint => &[
            Pane::View3d,
            Pane::View2d,
            Pane::Layers,
            Pane::Properties,
            Pane::MeshMaps,
            Pane::TextureSet,
            Pane::MeshSet,
            Pane::Preview,
            Pane::Shelf,
        ],
        Workspace::Designer => &[Pane::DesignView, Pane::Graph, Pane::NodeSettings],
        Workspace::ShaderMaker => &[Pane::ShaderList, Pane::ShaderCode, Pane::ShaderPreview],
    }
}

fn share(tiles: &mut Tiles<Pane>, container: TileId, child: TileId, share: f32) {
    if let Some(Tile::Container(Container::Linear(l))) = tiles.get_mut(container) {
        l.shares.set_share(child, share);
    }
}

/// The layout a workspace starts with.
fn default_tree(ws: Workspace) -> Tree<Pane> {
    let mut t = Tiles::default();
    let root = match ws {
        Workspace::Paint => {
            let v3 = t.insert_pane(Pane::View3d);
            let v2 = t.insert_pane(Pane::View2d);
            let v3 = t.insert_tab_tile(vec![v3]);
            let v2 = t.insert_tab_tile(vec![v2]);
            let views = t.insert_horizontal_tile(vec![v3, v2]);
            let shelf = t.insert_pane(Pane::Shelf);
            let shelf = t.insert_tab_tile(vec![shelf]);
            let left = t.insert_vertical_tile(vec![views, shelf]);
            share(&mut t, left, views, 4.0);
            share(&mut t, left, shelf, 1.0);
            let stack: Vec<TileId> = [Pane::Layers, Pane::MeshMaps, Pane::TextureSet, Pane::MeshSet, Pane::Preview].map(|p| t.insert_pane(p)).to_vec();
            let stack = t.insert_tab_tile(stack);
            let props = t.insert_pane(Pane::Properties);
            let props = t.insert_tab_tile(vec![props]);
            let right = t.insert_vertical_tile(vec![stack, props]);
            share(&mut t, right, stack, 1.0);
            share(&mut t, right, props, 1.4);
            let root = t.insert_horizontal_tile(vec![left, right]);
            share(&mut t, root, left, 3.2);
            share(&mut t, root, right, 1.0);
            root
        }
        Workspace::Designer => {
            let view = t.insert_pane(Pane::DesignView);
            let view = t.insert_tab_tile(vec![view]);
            let graph = t.insert_pane(Pane::Graph);
            let graph = t.insert_tab_tile(vec![graph]);
            let node = t.insert_pane(Pane::NodeSettings);
            let node = t.insert_tab_tile(vec![node]);
            let root = t.insert_horizontal_tile(vec![view, graph, node]);
            share(&mut t, root, view, 1.2);
            share(&mut t, root, graph, 2.8);
            share(&mut t, root, node, 1.0);
            root
        }
        Workspace::ShaderMaker => {
            let list = t.insert_pane(Pane::ShaderList);
            let list = t.insert_tab_tile(vec![list]);
            let code = t.insert_pane(Pane::ShaderCode);
            let code = t.insert_tab_tile(vec![code]);
            let preview = t.insert_pane(Pane::ShaderPreview);
            let preview = t.insert_tab_tile(vec![preview]);
            let root = t.insert_horizontal_tile(vec![list, code, preview]);
            share(&mut t, root, list, 0.8);
            share(&mut t, root, code, 3.0);
            share(&mut t, root, preview, 1.2);
            root
        }
    };
    Tree::new(egui::Id::unique(ws.name()), root, t)
}

pub(super) struct Docks {
    trees: [Tree<Pane>; 3],
    /// Changed since saved.
    dirty: bool,
}

fn layout_path() -> Option<std::path::PathBuf> {
    Some(super::project_io::data_dir()?.join("layout.json"))
}

fn index(ws: Workspace) -> usize {
    Workspace::ALL.iter().position(|w| *w == ws).expect("listed")
}

/// A saved layout is used only if it still holds exactly the panes the
/// workspace has (a newer version may have added some).
fn valid(tree: &Tree<Pane>, ws: Workspace) -> bool {
    panes(ws).iter().all(|p| tree.tiles.find_pane(p).is_some())
        && tree.tiles.tiles().filter(|t| matches!(t, Tile::Pane(_))).count() == panes(ws).len()
}

impl Docks {
    pub fn load() -> Docks {
        let saved: Option<Vec<Tree<Pane>>> =
            layout_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok());
        let trees = Workspace::ALL.map(|ws| {
            saved.as_ref().and_then(|s| s.get(index(ws)).cloned()).filter(|t| valid(t, ws)).unwrap_or_else(|| default_tree(ws))
        });
        Docks { trees, dirty: false }
    }

    /// Brings a pane's tab to the front.
    #[cfg(test)]
    pub fn show_pane(&mut self, ws: Workspace, pane: Pane) {
        let tree = &mut self.trees[index(ws)];
        tree.make_active(|_, tile| matches!(tile, Tile::Pane(p) if *p == pane));
    }

    pub fn reset(&mut self, ws: Workspace) {
        self.trees[index(ws)] = default_tree(ws);
        self.dirty = true;
    }

    /// Writes the layouts once a drag or resize has finished.
    pub fn save_if_changed(&mut self, ctx: &egui::Context) {
        if !self.dirty || ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        self.dirty = false;
        let Some(path) = layout_path() else { return };
        if let Ok(text) = serde_json::to_string(&self.trees) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, text);
        }
    }
}

struct DockBehavior<'a> {
    app: &'a mut PainterApp,
    edited: bool,
}

impl Behavior<Pane> for DockBehavior<'_> {
    fn pane_ui(&mut self, ui: &mut egui::Ui, _tile: TileId, pane: &mut Pane) -> UiResponse {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, if pane.padded() { theme::PANEL } else { theme::PANEL_DARK });
        let app = &mut *self.app;
        let draw = |ui: &mut egui::Ui, app: &mut PainterApp| match *pane {
            Pane::View3d | Pane::DesignView => app.view_pane(ui, ViewKind::V3d),
            Pane::View2d => app.view_pane(ui, ViewKind::V2d),
            Pane::Layers => app.layers_panel(ui),
            Pane::Properties => app.properties(ui),
            Pane::MeshMaps => app.mesh_maps_panel(ui),
            Pane::TextureSet => app.texture_set_panel(ui),
            Pane::MeshSet => app.mesh_set_panel(ui),
            Pane::Preview => app.preview_panel(ui),
            Pane::Shelf => app.shelf_panel(ui),
            Pane::Graph => app.designer_graph_pane(ui),
            Pane::NodeSettings => app.designer_settings_pane(ui),
            Pane::ShaderList => app.shader_list(ui),
            Pane::ShaderCode => app.shader_code_pane(ui),
            Pane::ShaderPreview => app.shader_preview_pane(ui),
        };
        let t0 = std::time::Instant::now();
        if pane.padded() {
            egui::Frame::new().inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                draw(ui, app);
            });
        } else {
            draw(ui, app);
        }
        if app.profile {
            app.prof.push((pane.title(), t0.elapsed().as_secs_f32() * 1000.0));
        }
        // A clear edge around every panel.
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, theme::OUTLINE), egui::StrokeKind::Inside);
        UiResponse::None
    }

    fn tab_title_for_pane(&mut self, pane: &Pane) -> egui::WidgetText {
        pane.title().into()
    }

    /// The display-mode spheres sit on the 3D view's own tab bar.
    fn top_bar_right_ui(&mut self, tiles: &Tiles<Pane>, ui: &mut egui::Ui, _tile: TileId, tabs: &egui_tiles::Tabs, _scroll: &mut f32) {
        if tabs.active.and_then(|id| tiles.get_pane(&id)) == Some(&Pane::View3d) {
            self.app.view_mode_strip(ui);
        }
    }

    fn is_tab_closable(&self, _tiles: &Tiles<Pane>, _tile: TileId) -> bool {
        false
    }

    fn simplification_options(&self) -> SimplificationOptions {
        SimplificationOptions { all_panes_must_have_tabs: true, ..Default::default() }
    }

    fn gap_width(&self, _style: &egui::Style) -> f32 {
        6.0
    }

    fn tab_bar_height(&self, _style: &egui::Style) -> f32 {
        28.0
    }

    fn tab_bar_color(&self, _visuals: &egui::Visuals) -> Color32 {
        theme::PANEL_DARK
    }

    fn tab_bg_color(&self, _visuals: &egui::Visuals, _tiles: &Tiles<Pane>, _tile: TileId, state: &TabState) -> Color32 {
        if state.active { theme::SELECT_BG } else { Color32::TRANSPARENT }
    }

    fn tab_outline_stroke(&self, _visuals: &egui::Visuals, _tiles: &Tiles<Pane>, _tile: TileId, state: &TabState) -> Stroke {
        if state.active { Stroke::new(1.0, theme::ACCENT) } else { Stroke::NONE }
    }

    fn tab_bar_hline_stroke(&self, _visuals: &egui::Visuals) -> Stroke {
        Stroke::new(1.0, theme::OUTLINE)
    }

    fn tab_text_color(&self, _visuals: &egui::Visuals, _tiles: &Tiles<Pane>, _tile: TileId, state: &TabState) -> Color32 {
        if state.active { Color32::WHITE } else { theme::TEXT_DIM }
    }

    fn resize_stroke(&self, _style: &egui::Style, state: egui_tiles::ResizeState) -> Stroke {
        match state {
            egui_tiles::ResizeState::Idle => Stroke::NONE,
            egui_tiles::ResizeState::Hovering => Stroke::new(2.0, theme::ACCENT.gamma_multiply(0.6)),
            egui_tiles::ResizeState::Dragging => Stroke::new(2.0, theme::ACCENT),
        }
    }

    fn on_edit(&mut self, _action: egui_tiles::EditAction) {
        self.edited = true;
    }
}

impl PainterApp {
    /// Lays out a workspace's docked panels in `ui`.
    pub(super) fn dock_ui(&mut self, ui: &mut egui::Ui, ws: Workspace) {
        let i = index(ws);
        let mut tree = std::mem::replace(&mut self.docks.trees[i], Tree::empty(egui::Id::unique("dock in use")));
        if ws == Workspace::Paint {
            // The view layout buttons (F1-F3) show and hide the viewports.
            let (show3, show2) = match self.layout {
                super::Layout::Split => (true, true),
                super::Layout::View3d => (true, false),
                super::Layout::View2d => (false, true),
            };
            for (pane, show) in [(Pane::View3d, show3), (Pane::View2d, show2)] {
                if let Some(id) = tree.tiles.find_pane(&pane) {
                    tree.set_visible(id, show);
                }
            }
        }
        let mut behavior = DockBehavior { app: self, edited: false };
        tree.ui(&mut behavior, ui);
        let edited = behavior.edited;
        self.docks.trees[i] = tree;
        self.docks.dirty |= edited;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layouts_are_complete_and_survive_saving() {
        for ws in Workspace::ALL {
            let tree = default_tree(ws);
            assert!(valid(&tree, ws), "{} default layout misses panes", ws.name());
            let text = serde_json::to_string(&vec![tree]).unwrap();
            let back: Vec<Tree<Pane>> = serde_json::from_str(&text).unwrap();
            assert!(valid(&back[0], ws), "{} layout broke in a save round trip", ws.name());
            // Another workspace's layout doesn't pass for this one.
            let other = Workspace::ALL.iter().copied().find(|w| *w != ws).unwrap();
            assert!(!valid(&back[0], other));
        }
    }
}
