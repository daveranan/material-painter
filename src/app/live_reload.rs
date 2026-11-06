//! Live reload of text projects: when `name.json` or a file in `name.data/`
//! changes on disk (an editor, a script, an AI agent), the open document
//! follows. Layer, graph and view edits apply in place as one undo step;
//! mesh or bake changes reopen the project.
//!
//! The app's own saves must not reload. Each state the document matches is
//! identified by a fingerprint of the manifest and the data files it refers
//! to; a save records its fingerprint before writing, and a change on disk
//! only reloads when the fingerprint differs. Per-file hashes tell which
//! data files changed, so a PNG rewritten under the same name (a regenerated
//! mask) reloads the texture sets that use it.

use super::PainterApp;
use super::project_io::{data_dir_of, data_path, is_text_project};
use eframe::egui;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Manifest keys whose string values are data-folder paths.
const FILE_KEYS: [&str; 6] = ["blob", "data", "vertices", "indices", "tri_object", "tri_color"];

#[derive(Default)]
pub(super) struct LiveProject {
    path: Option<PathBuf>,
    /// The manifest the document matches, to tell what an edit changed.
    manifest: Value,
    /// Fingerprint of the files the document matches.
    seen: Fingerprint,
    /// Something changed on disk since the last check.
    pending: bool,
    _watcher: Option<notify::RecommendedWatcher>,
    changes: Option<mpsc::Receiver<()>>,
    /// Why the last change on disk couldn't be applied.
    pub(super) error: Option<String>,
    /// Wakes the UI when a change arrives.
    ctx: Option<egui::Context>,
}

/// A text project's state on disk: one hash for the manifest and every data
/// file it refers to, and one per data file.
#[derive(Default, Clone, PartialEq, Debug)]
pub(super) struct Fingerprint {
    all: u64,
    files: HashMap<String, u64>,
}

/// Identifies a manifest and the files it refers to (sorted by path).
pub(super) fn fingerprint<'a>(manifest: &[u8], files: impl Iterator<Item = (&'a str, &'a [u8])>) -> Fingerprint {
    let mut files: Vec<_> = files.collect();
    files.sort_by_key(|(rel, _)| *rel);
    let mut h = std::hash::DefaultHasher::new();
    manifest.hash(&mut h);
    let mut each = HashMap::with_capacity(files.len());
    for (rel, bytes) in files {
        let mut fh = std::hash::DefaultHasher::new();
        bytes.hash(&mut fh);
        let fh = fh.finish();
        rel.hash(&mut h);
        fh.hash(&mut h);
        each.insert(rel.to_string(), fh);
    }
    Fingerprint { all: h.finish(), files: each }
}

/// Whether `v` (part of a manifest) refers to any of `files`.
pub(super) fn refers_to_any(v: &Value, files: &HashSet<String>) -> bool {
    if files.is_empty() {
        return false;
    }
    let mut refs = Vec::new();
    file_refs(v, &mut refs);
    refs.iter().any(|r| files.contains(*r))
}

fn file_refs<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                match x.as_str() {
                    Some(rel) if FILE_KEYS.contains(&k.as_str()) => out.push(rel),
                    _ => file_refs(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| file_refs(x, out)),
        _ => {}
    }
}

/// The fingerprint of a text project as it is on disk now.
fn disk_fingerprint(path: &Path) -> Result<Fingerprint, String> {
    let text = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let manifest: Value = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let dir = data_dir_of(path);
    let mut refs = Vec::new();
    file_refs(&manifest, &mut refs);
    let mut files = Vec::with_capacity(refs.len());
    for rel in refs {
        // A missing file still fingerprints (as empty); loading reports it.
        let bytes = data_path(&dir, rel).ok().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
        files.push((rel, bytes));
    }
    Ok(fingerprint(&text, files.iter().map(|(r, b)| (*r, b.as_slice()))))
}

impl LiveProject {
    pub(super) fn new(ctx: &egui::Context) -> LiveProject {
        LiveProject { ctx: Some(ctx.clone()), ..Default::default() }
    }

    /// Follows `path` if it's a text project (stops following otherwise);
    /// `manifest` is what the document now matches.
    pub(super) fn watch(&mut self, path: &Path, manifest: Value) {
        if !is_text_project(path) {
            *self = LiveProject { ctx: self.ctx.take(), ..Default::default() };
            return;
        }
        if self.path.as_deref() != Some(path) {
            *self = LiveProject { ctx: self.ctx.take(), ..Default::default() };
            self.start_watcher(path);
            self.path = Some(path.to_path_buf());
        }
        self.manifest = manifest;
        self.error = None;
    }

    /// The document matches files with this fingerprint (a save is about
    /// to write them, or they were just loaded).
    pub(super) fn expect(&mut self, fingerprint: Fingerprint) {
        self.seen = fingerprint;
    }

    /// Data files whose contents differ between what the document matches and `now`.
    fn changed_files(&self, now: &Fingerprint) -> HashSet<String> {
        now.files.iter().filter(|(rel, h)| self.seen.files.get(*rel) != Some(*h)).map(|(rel, _)| rel.clone()).collect()
    }

    pub(super) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn start_watcher(&mut self, path: &Path) {
        use notify::Watcher;
        let (Some(parent), dir) = (path.parent(), data_dir_of(path)) else { return };
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = mpsc::channel();
        let (manifest, data) = (path.to_path_buf(), dir.clone());
        let ctx = self.ctx.clone();
        let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(e) = res else { return };
            if matches!(e.kind, notify::EventKind::Access(_)) {
                return;
            }
            // Temporary files of atomic writes don't count, their rename does.
            let relevant = |p: &PathBuf| (p == &manifest || p.starts_with(&data)) && !p.extension().is_some_and(|x| x == "tmp");
            if e.paths.iter().any(relevant) {
                let _ = tx.send(());
                if let Some(ctx) = &ctx {
                    ctx.request_repaint();
                }
            }
        });
        if let Ok(mut w) = w {
            let ok = w.watch(parent, notify::RecursiveMode::NonRecursive).is_ok() && w.watch(&dir, notify::RecursiveMode::Recursive).is_ok();
            if ok {
                self._watcher = Some(w);
                self.changes = Some(rx);
            }
        }
    }
}

impl PainterApp {
    /// Applies changes made to the open text project on disk.
    pub(super) fn process_live_reload(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.live.changes {
            if rx.try_iter().count() > 0 {
                self.live.pending = true;
            }
        }
        // Not in the middle of a stroke or a bake; the next frame retries.
        if !self.live.pending || self.stroke.is_some() || self.bake_job.is_some() {
            return;
        }
        self.live.pending = false;
        let Some(path) = self.live.path.clone() else { return };
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let fp = match disk_fingerprint(&path) {
            Ok(fp) if fp.all == self.live.seen.all => return,
            Ok(fp) => fp,
            Err(e) => {
                // Usually an editor mid-save or a typo; the next change retries.
                self.status = format!("Not reloaded: {e}");
                self.live.error = Some(e);
                return;
            }
        };
        let before = self.live.manifest.clone();
        let changed = self.live.changed_files(&fp);
        let result = match self.reload_in_place(&path, &before, &changed, ctx) {
            Ok(true) => Ok(format!("Reloaded {name} from disk (Ctrl+Z undoes it).")),
            Ok(false) => self.open_project(&path, ctx).map(|()| format!("Reopened {name}: its mesh or bake settings changed on disk.")),
            Err(e) => Err(e),
        };
        match result {
            Ok(msg) => {
                self.live.expect(fp);
                self.live.error = None;
                self.saved_serial = self.undo.state_serial();
                self.status = msg;
            }
            Err(e) => {
                // Remember the broken state so it isn't retried every frame.
                self.live.expect(fp);
                self.status = format!("Not reloaded, {name} has an error: {e}");
                self.live.error = Some(e);
            }
        }
    }

    /// Live-reload state for `get_state`.
    pub(super) fn live_reload_json(&self) -> Value {
        serde_json::json!({
            "watching": self.live.path().map(|p| p.display().to_string()),
            "error": self.live.error,
        })
    }
}

/// After opening a text project: what's on disk is what the document has.
pub(super) fn mark_loaded(live: &mut LiveProject, path: &Path) {
    if live.path() == Some(path) {
        if let Ok(fp) = disk_fingerprint(path) {
            live.expect(fp);
        }
    }
}

#[cfg(test)]
impl LiveProject {
    pub(crate) fn seen_for_test(&self) -> u64 {
        self.seen.all
    }
}

#[cfg(test)]
pub(crate) fn disk_fingerprint_for_test(path: &Path) -> u64 {
    disk_fingerprint(path).expect("readable project").all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewritten_files_are_found() {
        let a = fingerprint(b"{}", [("x.png", &b"one"[..]), ("y.png", &b"two"[..])].into_iter());
        let b = fingerprint(b"{}", [("x.png", &b"one"[..]), ("y.png", &b"new"[..])].into_iter());
        assert_ne!(a.all, b.all);
        let live = LiveProject { seen: a, ..Default::default() };
        assert_eq!(live.changed_files(&b), HashSet::from(["y.png".to_string()]));
        let set: Value = serde_json::json!({"layers": [{"mask": {"paint": {"blob": "y.png"}}}]});
        assert!(refers_to_any(&set, &live.changed_files(&b)));
        assert!(!refers_to_any(&set, &HashSet::from(["x.png".to_string()])));
    }
}
