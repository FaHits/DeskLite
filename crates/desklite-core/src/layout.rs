use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::clamp::{
    MIN_FENCE_HEIGHT, MIN_FENCE_WIDTH, clamp_point_to_monitors, clamp_rect_to_monitors,
    clamp_rect_to_work_area, nearest_monitor,
};
use crate::topology::{
    DisplayTopology, MonitorDescriptor, Point, Rect, TopologyError, TopologyKey,
};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_BACKDROP_ALPHA: u8 = 115;

const fn default_backdrop_alpha() -> u8 {
    DEFAULT_BACKDROP_ALPHA
}

const fn default_cell_w() -> i32 {
    84
}

const fn default_cell_h() -> i32 {
    92
}

const fn default_snap_px() -> i32 {
    12
}

const fn default_list_row_h() -> i32 {
    30
}

const fn default_list_icon_px() -> i32 {
    18
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceKind {
    IconGrid,
    FileList,
    Unknown(i32),
}

impl From<i32> for FenceKind {
    fn from(value: i32) -> Self {
        match value {
            3 => Self::IconGrid,
            1 => Self::FileList,
            other => Self::Unknown(other),
        }
    }
}

impl From<FenceKind> for i32 {
    fn from(value: FenceKind) -> Self {
        match value {
            FenceKind::IconGrid => 3,
            FenceKind::FileList => 1,
            FenceKind::Unknown(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    VirtualShell,
    Shortcut,
    Folder,
    File,
    Unknown(u32),
}

impl From<u32> for ItemKind {
    fn from(value: u32) -> Self {
        match value {
            1 => Self::VirtualShell,
            2 => Self::Shortcut,
            3 => Self::Folder,
            4 => Self::File,
            other => Self::Unknown(other),
        }
    }
}

impl From<ItemKind> for u32 {
    fn from(value: ItemKind) -> Self {
        match value {
            ItemKind::VirtualShell => 1,
            ItemKind::Shortcut => 2,
            ItemKind::Folder => 3,
            ItemKind::File => 4,
            ItemKind::Unknown(v) => v,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceItem {
    pub display_name: String,
    pub order: i32,
    pub kind: ItemKind,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceLayoutOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_cell_w: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_cell_h: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_row_h: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_icon_px: Option<i32>,
}

impl FenceLayoutOverride {
    pub fn is_empty(&self) -> bool {
        self.icon_cell_w.is_none()
            && self.icon_cell_h.is_none()
            && self.list_row_h.is_none()
            && self.list_icon_px.is_none()
    }
}

/// Normalizes a filesystem path into a case-insensitive key without trailing separators.
pub fn normalize_path_key(path: &str) -> String {
    let trimmed = path.trim();
    let stripped = trimmed.strip_prefix(r"\\?\").unwrap_or(trimmed);
    let slashed = stripped.replace('/', "\\");
    let without_trailing = slashed.trim_end_matches('\\');
    if without_trailing.len() == 2 && without_trailing.ends_with(':') {
        format!("{}\\", without_trailing.to_ascii_lowercase())
    } else {
        without_trailing.to_ascii_lowercase()
    }
}

fn is_desktop_dir_str(dir: &str, desktop_dirs: &[String]) -> bool {
    let key = normalize_path_key(dir);
    !key.is_empty()
        && desktop_dirs
            .iter()
            .any(|d| !d.trim().is_empty() && normalize_path_key(d) == key)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceRole {
    /// Hosts categorized desktop files in-place on the Desktop (or legacy vault paths).
    DesktopBucket,
    /// Legacy alias from earlier builds; always resolves/normalizes to `DesktopBucket`
    /// so `FreeDesktop` (`BoxId=1`) owns unfiled desktop files directly without conflict.
    UnfiledDesktop,
    /// Browses an external non-desktop folder portal.
    ExternalPortal { dir: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceSpec {
    pub id: i32,
    pub name: String,
    pub kind: FenceKind,
    pub rect: Rect,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<FenceRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal_dir: Option<String>,
    #[serde(default = "default_backdrop_alpha")]
    pub backdrop_alpha: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_override: Option<FenceLayoutOverride>,
    pub items: Vec<FenceItem>,
}

impl FenceSpec {
    /// Resolves the effective role using `role` first, falling back to `portal_dir`.
    /// Any legacy `UnfiledDesktop` or `portal_dir` pointing to a desktop directory resolves to
    /// `FenceRole::DesktopBucket` so desktop fences remain mutually exclusive with `FreeDesktop`.
    pub fn effective_role(&self, desktop_dirs: &[String]) -> FenceRole {
        if let Some(role) = &self.role {
            return match role {
                FenceRole::UnfiledDesktop => FenceRole::DesktopBucket,
                FenceRole::ExternalPortal { dir }
                    if dir.trim().is_empty() || is_desktop_dir_str(dir, desktop_dirs) =>
                {
                    FenceRole::DesktopBucket
                }
                other => other.clone(),
            };
        }
        match self
            .portal_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(dir) if is_desktop_dir_str(dir, desktop_dirs) => FenceRole::DesktopBucket,
            Some(dir) => FenceRole::ExternalPortal {
                dir: dir.to_string(),
            },
            None => FenceRole::DesktopBucket,
        }
    }

    /// Normalizes legacy `portal_dir` and `UnfiledDesktop` into `DesktopBucket` (preserving
    /// `items` and `rect`), keeping `portal_dir` populated only for external folder portals.
    pub fn normalize_role(&mut self, desktop_dirs: &[String]) {
        let resolved = self.effective_role(desktop_dirs);
        self.role = Some(resolved.clone());
        match resolved {
            FenceRole::ExternalPortal { dir } => {
                self.portal_dir = Some(dir);
            }
            FenceRole::DesktopBucket | FenceRole::UnfiledDesktop => {
                self.portal_dir = None;
            }
        }
    }

    pub fn is_desktop_bucket(&self, desktop_dirs: &[String]) -> bool {
        matches!(self.effective_role(desktop_dirs), FenceRole::DesktopBucket)
    }

    pub fn is_unfiled_desktop(&self, desktop_dirs: &[String]) -> bool {
        matches!(self.effective_role(desktop_dirs), FenceRole::UnfiledDesktop)
    }

    pub fn external_portal_dir(&self, desktop_dirs: &[String]) -> Option<String> {
        match self.effective_role(desktop_dirs) {
            FenceRole::ExternalPortal { dir } => Some(dir),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPreferences {
    #[serde(default = "default_cell_w")]
    pub icon_cell_w: i32,
    #[serde(default = "default_cell_h")]
    pub icon_cell_h: i32,
    #[serde(default = "default_snap_px")]
    pub snap_threshold_px: i32,
    #[serde(default = "default_list_row_h")]
    pub list_row_h: i32,
    #[serde(default = "default_list_icon_px")]
    pub list_icon_px: i32,
}

impl Default for LayoutPreferences {
    fn default() -> Self {
        Self {
            icon_cell_w: default_cell_w(),
            icon_cell_h: default_cell_h(),
            snap_threshold_px: default_snap_px(),
            list_row_h: default_list_row_h(),
            list_icon_px: default_list_icon_px(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopIconPos {
    pub id: String,
    pub display_name: String,
    pub pos: Point,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    pub topology_key: TopologyKey,
    pub monitors: Vec<MonitorDescriptor>,
    pub fences: BTreeMap<i32, Rect>,
    pub free_icons: Vec<DesktopIconPos>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeLayoutConfig {
    pub schema_version: u32,
    pub active_topology: Option<TopologyKey>,
    #[serde(default)]
    pub hide_all_fences: bool,
    #[serde(default)]
    pub preferences: LayoutPreferences,
    pub fences: Vec<FenceSpec>,
    pub snapshots: BTreeMap<TopologyKey, TopologySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub topology_key: TopologyKey,
    pub restored_from_snapshot: bool,
    pub clamped_fences: usize,
    pub clamped_icons: usize,
}

impl Default for NativeLayoutConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            active_topology: None,
            hide_all_fences: false,
            preferences: LayoutPreferences::default(),
            fences: Vec::new(),
            snapshots: BTreeMap::new(),
        }
    }
}

impl NativeLayoutConfig {
    pub fn next_fence_id(&self) -> i32 {
        self.fences
            .iter()
            .map(|f| f.id)
            .filter(|&id| id >= 100)
            .max()
            .unwrap_or(100)
            + 1
    }

    pub fn fence_mut(&mut self, id: i32) -> Option<&mut FenceSpec> {
        self.fences.iter_mut().find(|f| f.id == id)
    }

    pub fn upsert_fence(&mut self, fence: FenceSpec) {
        if let Some(slot) = self.fences.iter_mut().find(|f| f.id == fence.id) {
            *slot = fence;
        } else {
            self.fences.push(fence);
        }
    }

    pub fn remove_fence(&mut self, id: i32) -> bool {
        let before = self.fences.len();
        self.fences.retain(|f| f.id != id);
        for snap in self.snapshots.values_mut() {
            snap.fences.remove(&id);
        }
        self.fences.len() != before
    }

    pub fn update_active_snapshot_rect(&mut self, id: i32, rect: Rect) {
        if let Some(key) = &self.active_topology
            && let Some(snap) = self.snapshots.get_mut(key)
        {
            snap.fences.insert(id, rect);
        }
    }

    /// Removes DeskGo's internal hidden full-screen container box (`id == 1` with empty name)
    /// if it leaked into a previously imported `layout.json`.
    pub fn sanitize_legacy_containers(&mut self) -> bool {
        let mut removed = false;
        let before = self.fences.len();
        self.fences
            .retain(|f| !(f.id == 1 && f.name.trim().is_empty()));
        if self.fences.len() != before {
            removed = true;
        }
        for snap in self.snapshots.values_mut() {
            if !self.fences.iter().any(|f| f.id == 1) && snap.fences.remove(&1).is_some() {
                removed = true;
            }
        }
        removed
    }
    pub fn sanitize_bounds(&mut self) {
        self.preferences.icon_cell_w = self.preferences.icon_cell_w.clamp(24, 320);
        self.preferences.icon_cell_h = self.preferences.icon_cell_h.clamp(24, 320);
        self.preferences.snap_threshold_px = self.preferences.snap_threshold_px.clamp(0, 64);
        self.preferences.list_row_h = self.preferences.list_row_h.clamp(16, 128);
        self.preferences.list_icon_px = self.preferences.list_icon_px.clamp(12, 96);

        for fence in &mut self.fences {
            let w = fence.rect.width().clamp(MIN_FENCE_WIDTH, 16384);
            let h = fence.rect.height().clamp(MIN_FENCE_HEIGHT, 16384);
            fence.rect = Rect::new(
                fence.rect.left,
                fence.rect.top,
                fence.rect.left + w,
                fence.rect.top + h,
            );
            if let Some(ov) = &mut fence.layout_override {
                ov.icon_cell_w = ov.icon_cell_w.map(|v| v.clamp(24, 320));
                ov.icon_cell_h = ov.icon_cell_h.map(|v| v.clamp(24, 320));
                ov.list_row_h = ov.list_row_h.map(|v| v.clamp(16, 128));
                ov.list_icon_px = ov.list_icon_px.map(|v| v.clamp(12, 96));
            }
        }
    }

    pub fn active_free_icons(&self) -> &[DesktopIconPos] {
        self.active_topology
            .as_ref()
            .and_then(|k| self.snapshots.get(k))
            .map(|s| s.free_icons.as_slice())
            .unwrap_or(&[])
    }

    pub fn active_free_icons_mut(&mut self) -> Option<&mut Vec<DesktopIconPos>> {
        let key = self.active_topology.as_ref()?;
        self.snapshots.get_mut(key).map(|s| &mut s.free_icons)
    }

    pub fn normalize_roles(&mut self, desktop_dirs: &[String]) {
        for fence in &mut self.fences {
            fence.normalize_role(desktop_dirs);
        }
    }

    pub fn unfiled_desktop_fence_id(&self, desktop_dirs: &[String]) -> Option<i32> {
        self.fences
            .iter()
            .find(|f| f.is_unfiled_desktop(desktop_dirs))
            .map(|f| f.id)
    }

    /// Returns the ID of the `DesktopBucket` fence currently owning `path`, if any.
    pub fn desktop_owner_of(&self, path: &str, desktop_dirs: &[String]) -> Option<i32> {
        let target = normalize_path_key(path);
        if target.is_empty() {
            return None;
        }
        self.fences
            .iter()
            .find(|f| {
                f.is_desktop_bucket(desktop_dirs)
                    && f.items
                        .iter()
                        .any(|it| normalize_path_key(&it.path) == target)
            })
            .map(|f| f.id)
    }

    /// Returns normalized keys of all items owned by any `DesktopBucket` fence.
    pub fn all_bucket_desktop_paths(&self, desktop_dirs: &[String]) -> BTreeSet<String> {
        let mut owned = BTreeSet::new();
        for fence in &self.fences {
            if fence.is_desktop_bucket(desktop_dirs) {
                for item in &fence.items {
                    let key = normalize_path_key(&item.path);
                    if !key.is_empty() {
                        owned.insert(key);
                    }
                }
            }
        }
        owned
    }

    /// Enforces single-owner invariant across `DesktopBucket` fences and purges owned items
    /// from `snapshots[*].free_icons`. Returns the count of removed duplicate/conflicting entries.
    pub fn enforce_desktop_single_ownership(&mut self, desktop_dirs: &[String]) -> usize {
        let mut removed = 0usize;
        let mut seen = BTreeSet::new();
        for fence in &mut self.fences {
            if !fence.is_desktop_bucket(desktop_dirs) {
                continue;
            }
            let before = fence.items.len();
            fence.items.retain(|it| {
                let key = normalize_path_key(&it.path);
                !key.is_empty() && seen.insert(key)
            });
            if fence.items.len() != before {
                removed += before - fence.items.len();
                reindex_fence_items(&mut fence.items);
            }
        }
        for snap in self.snapshots.values_mut() {
            let before = snap.free_icons.len();
            snap.free_icons
                .retain(|icon| !seen.contains(&normalize_path_key(&icon.id)));
            removed += before - snap.free_icons.len();
        }
        removed
    }

    /// Assigns `items` exclusively to `target_fence_id` (must resolve to `FenceRole::DesktopBucket`),
    /// stripping matching paths from all other `DesktopBucket` fences and all `free_icons` snapshots.
    pub fn assign_desktop_items_to_bucket(
        &mut self,
        target_fence_id: i32,
        items: Vec<FenceItem>,
        desktop_dirs: &[String],
    ) -> bool {
        let is_valid_bucket = self
            .fences
            .iter()
            .any(|f| f.id == target_fence_id && f.is_desktop_bucket(desktop_dirs));
        if !is_valid_bucket {
            return false;
        }

        let mut incoming_keys = BTreeSet::new();
        let mut deduped_incoming = Vec::with_capacity(items.len());
        for item in items {
            let key = normalize_path_key(&item.path);
            if !key.is_empty() && incoming_keys.insert(key) {
                deduped_incoming.push(item);
            }
        }
        if incoming_keys.is_empty() {
            return true;
        }

        for fence in &mut self.fences {
            if fence.id != target_fence_id && fence.is_desktop_bucket(desktop_dirs) {
                let before = fence.items.len();
                fence
                    .items
                    .retain(|it| !incoming_keys.contains(&normalize_path_key(&it.path)));
                if fence.items.len() != before {
                    reindex_fence_items(&mut fence.items);
                }
            }
        }

        for snap in self.snapshots.values_mut() {
            snap.free_icons
                .retain(|icon| !incoming_keys.contains(&normalize_path_key(&icon.id)));
        }

        if let Some(target) = self.fences.iter_mut().find(|f| f.id == target_fence_id) {
            let mut existing: BTreeSet<String> = target
                .items
                .iter()
                .map(|it| normalize_path_key(&it.path))
                .collect();
            for item in deduped_incoming {
                let key = normalize_path_key(&item.path);
                if existing.insert(key) {
                    target.items.push(item);
                }
            }
            reindex_fence_items(&mut target.items);
        }
        true
    }

    /// Releases `paths` from `from_fence_id` (if it is a `DesktopBucket`) and re-indexes item order.
    pub fn release_desktop_items(
        &mut self,
        from_fence_id: i32,
        paths: &[String],
        desktop_dirs: &[String],
    ) -> usize {
        let remove_keys: BTreeSet<String> = paths
            .iter()
            .map(|p| normalize_path_key(p))
            .filter(|k| !k.is_empty())
            .collect();
        if remove_keys.is_empty() {
            return 0;
        }
        let Some(fence) = self
            .fences
            .iter_mut()
            .find(|f| f.id == from_fence_id && f.is_desktop_bucket(desktop_dirs))
        else {
            return 0;
        };
        let before = fence.items.len();
        fence
            .items
            .retain(|it| !remove_keys.contains(&normalize_path_key(&it.path)));
        let removed = before - fence.items.len();
        if removed > 0 {
            reindex_fence_items(&mut fence.items);
        }
        removed
    }

    /// Removes `path` from every `DesktopBucket` fence and every snapshot's `free_icons`.
    pub fn remove_desktop_path_everywhere(
        &mut self,
        path: &str,
        desktop_dirs: &[String],
    ) -> bool {
        let key = normalize_path_key(path);
        if key.is_empty() {
            return false;
        }
        let mut removed = false;
        for fence in &mut self.fences {
            if fence.is_desktop_bucket(desktop_dirs) {
                let before = fence.items.len();
                fence
                    .items
                    .retain(|it| normalize_path_key(&it.path) != key);
                if fence.items.len() != before {
                    removed = true;
                    reindex_fence_items(&mut fence.items);
                }
            }
        }
        for snap in self.snapshots.values_mut() {
            let before = snap.free_icons.len();
            snap.free_icons
                .retain(|icon| normalize_path_key(&icon.id) != key);
            if snap.free_icons.len() != before {
                removed = true;
            }
        }
        removed
    }

    /// Upserts a free desktop icon into the active topology snapshot unless currently owned
    /// by a `DesktopBucket` fence.
    pub fn upsert_free_icon(&mut self, icon: DesktopIconPos, desktop_dirs: &[String]) -> bool {
        let key = normalize_path_key(&icon.id);
        if key.is_empty() || self.desktop_owner_of(&icon.id, desktop_dirs).is_some() {
            return false;
        }
        let Some(free_icons) = self.active_free_icons_mut() else {
            return false;
        };
        if let Some(existing) = free_icons
            .iter_mut()
            .find(|entry| normalize_path_key(&entry.id) == key)
        {
            *existing = icon;
        } else {
            free_icons.push(icon);
        }
        true
    }

    /// Removes `path` from the active topology snapshot's `free_icons`.
    pub fn remove_free_icon(&mut self, path: &str) -> bool {
        let key = normalize_path_key(path);
        if key.is_empty() {
            return false;
        }
        let Some(free_icons) = self.active_free_icons_mut() else {
            return false;
        };
        let before = free_icons.len();
        free_icons.retain(|icon| normalize_path_key(&icon.id) != key);
        free_icons.len() != before
    }

    /// Reconciles the active snapshot's `free_icons` against physical desktop items:
    /// removes entries missing from disk or now owned by a `DesktopBucket`, and assigns
    /// non-overlapping grid positions to newly discovered unfiled desktop items.
    pub fn reconcile_free_icons(
        &mut self,
        scanned_desktop_items: &[FenceItem],
        origin: Point,
        cell_size: (i32, i32),
        monitors: &[MonitorDescriptor],
        desktop_dirs: &[String],
    ) -> Result<usize, TopologyError> {
        if monitors.is_empty() {
            return Err(TopologyError::EmptyTransient);
        }
        let owned = self.all_bucket_desktop_paths(desktop_dirs);
        let Some(active_key) = self.active_topology.clone() else {
            return Ok(0);
        };
        let Some(snap) = self.snapshots.get_mut(&active_key) else {
            return Ok(0);
        };

        let mut scanned_map: BTreeMap<String, &FenceItem> = BTreeMap::new();
        let mut scanned_ordered: Vec<(String, &FenceItem)> = Vec::new();
        for item in scanned_desktop_items {
            let key = normalize_path_key(&item.path);
            if !key.is_empty() && !scanned_map.contains_key(&key) {
                scanned_map.insert(key.clone(), item);
                scanned_ordered.push((key, item));
            }
        }

        let before = snap.free_icons.len();
        let mut kept_keys = BTreeSet::new();
        snap.free_icons.retain(|icon| {
            let key = normalize_path_key(&icon.id);
            !key.is_empty()
                && !owned.contains(&key)
                && scanned_map.contains_key(&key)
                && kept_keys.insert(key)
        });
        let removed = before - snap.free_icons.len();

        for icon in &mut snap.free_icons {
            let key = normalize_path_key(&icon.id);
            if let Some(scanned) = scanned_map.get(&key) {
                icon.display_name = scanned.display_name.clone();
            }
        }

        let step_x = cell_size.0.max(24);
        let step_y = cell_size.1.max(24);
        let primary = monitors
            .iter()
            .find(|m| m.is_primary)
            .unwrap_or(&monitors[0]);
        let work = primary.work_area;
        let start_x = origin.x.clamp(work.left, (work.right - step_x).max(work.left));
        let start_y = origin.y.clamp(work.top, (work.bottom - step_y).max(work.top));
        let max_cols = ((work.right - start_x).max(step_x) / step_x).max(1);
        let max_rows = ((work.bottom - start_y).max(step_y) / step_y).max(1);

        let mut added = 0usize;
        let mut slot_idx = 0usize;
        for (key, item) in scanned_ordered {
            if owned.contains(&key) || !kept_keys.insert(key) {
                continue;
            }
            let total_slots = (max_cols * max_rows) as usize;
            let mut chosen = None;
            for offset in 0..total_slots {
                let idx = (slot_idx + offset) % total_slots;
                let col = (idx / max_rows as usize) as i32;
                let row = (idx % max_rows as usize) as i32;
                let candidate = Point {
                    x: start_x + col * step_x,
                    y: start_y + row * step_y,
                };
                let collides = snap.free_icons.iter().any(|other| {
                    (other.pos.x - candidate.x).abs() < step_x / 2
                        && (other.pos.y - candidate.y).abs() < step_y / 2
                });
                if !collides {
                    chosen = Some(candidate);
                    slot_idx = idx + 1;
                    break;
                }
            }
            let raw_pos = chosen.unwrap_or_else(|| {
                let col = ((slot_idx / max_rows as usize) as i32) % max_cols;
                let row = (slot_idx % max_rows as usize) as i32;
                slot_idx += 1;
                Point {
                    x: start_x + col * step_x,
                    y: start_y + row * step_y,
                }
            });
            let (pos, _) = clamp_point_to_monitors(raw_pos, (step_x, step_y), monitors)?;
            snap.free_icons.push(DesktopIconPos {
                id: item.path.clone(),
                display_name: item.display_name.clone(),
                pos,
            });
            added += 1;
        }

        Ok(added + removed)
    }

    /// Applies a verified non-empty topology:
    /// - Restores per-topology fence & free-icon coordinates if previously seen.
    /// - For unseen topologies, migrates rects/icons from `active_topology`'s snapshot using
    ///   `device_key` monitor matching and DPI ratio scaling before clamping.
    /// - Always validates and clamps every fence and icon into active work areas (even on 1-monitor wakeup).
    /// - Unconditionally updates `active_topology` and refreshes the snapshot for this topology.
    pub fn apply_topology(
        &mut self,
        topology: &DisplayTopology,
        icon_margin: (i32, i32),
    ) -> Result<MigrationReport, TopologyError> {
        let key = topology.key().clone();
        let monitors = topology.monitors();
        let existing_snapshot = self.snapshots.get(&key).cloned();
        let restored_from_snapshot = existing_snapshot.is_some();
        let source_snapshot = if restored_from_snapshot {
            None
        } else {
            self.active_topology
                .as_ref()
                .and_then(|k| self.snapshots.get(k))
                .filter(|s| !s.monitors.is_empty())
                .cloned()
        };

        let mut clamped_fences = 0usize;
        let mut fence_rects = BTreeMap::new();
        for fence in &mut self.fences {
            let (clamped, changed) = if let Some(snap) = existing_snapshot.as_ref() {
                let candidate = snap.fences.get(&fence.id).copied().unwrap_or(fence.rect);
                clamp_rect_to_monitors(candidate, monitors)?
            } else if let Some(src_snap) = source_snapshot.as_ref() {
                let candidate = src_snap
                    .fences
                    .get(&fence.id)
                    .copied()
                    .unwrap_or(fence.rect);
                migrate_rect_for_unseen_topology(candidate, &src_snap.monitors, monitors)?
            } else {
                clamp_rect_to_monitors(fence.rect, monitors)?
            };
            if changed {
                clamped_fences += 1;
            }
            fence.rect = clamped;
            fence_rects.insert(fence.id, clamped);
        }

        let mut clamped_icons = 0usize;
        let mut free_icons = if let Some(snap) = existing_snapshot {
            snap.free_icons
        } else if let Some(src_snap) = source_snapshot.as_ref() {
            src_snap.free_icons.clone()
        } else {
            Vec::new()
        };
        for icon in &mut free_icons {
            let (clamped, changed) = if restored_from_snapshot {
                clamp_point_to_monitors(icon.pos, icon_margin, monitors)?
            } else if let Some(src_snap) = source_snapshot.as_ref() {
                migrate_point_for_unseen_topology(
                    icon.pos,
                    icon_margin,
                    &src_snap.monitors,
                    monitors,
                )?
            } else {
                clamp_point_to_monitors(icon.pos, icon_margin, monitors)?
            };
            if changed {
                clamped_icons += 1;
            }
            icon.pos = clamped;
        }

        self.active_topology = Some(key.clone());
        self.snapshots.insert(
            key.clone(),
            TopologySnapshot {
                topology_key: key.clone(),
                monitors: monitors.to_vec(),
                fences: fence_rects,
                free_icons,
            },
        );

        Ok(MigrationReport {
            topology_key: key,
            restored_from_snapshot,
            clamped_fences,
            clamped_icons,
        })
    }
}

fn reindex_fence_items(items: &mut [FenceItem]) {
    for (idx, item) in items.iter_mut().enumerate() {
        item.order = idx as i32;
    }
}

fn scale_by_dpi(value: i32, dst_dpi: u32, src_dpi: u32) -> i32 {
    if dst_dpi == src_dpi || src_dpi == 0 {
        return value;
    }
    (f64::from(value) * (f64::from(dst_dpi) / f64::from(src_dpi))).round() as i32
}

fn migrate_rect_for_unseen_topology(
    rect: Rect,
    src_monitors: &[MonitorDescriptor],
    dst_monitors: &[MonitorDescriptor],
) -> Result<(Rect, bool), TopologyError> {
    if dst_monitors.is_empty() {
        return Err(TopologyError::EmptyTransient);
    }
    let src_mon = nearest_monitor(rect.center(), src_monitors);
    let src_dpi_x = src_mon.dpi_x.max(96);
    let src_dpi_y = src_mon.dpi_y.max(96);

    if let Some(target) = dst_monitors
        .iter()
        .find(|m| m.device_key == src_mon.device_key)
    {
        let dst_dpi_x = target.dpi_x.max(96);
        let dst_dpi_y = target.dpi_y.max(96);
        let rel_x = scale_by_dpi(rect.left - src_mon.work_area.left, dst_dpi_x, src_dpi_x);
        let rel_y = scale_by_dpi(rect.top - src_mon.work_area.top, dst_dpi_y, src_dpi_y);
        let scaled_w = scale_by_dpi(rect.width(), dst_dpi_x, src_dpi_x).max(MIN_FENCE_WIDTH);
        let scaled_h = scale_by_dpi(rect.height(), dst_dpi_y, src_dpi_y).max(MIN_FENCE_HEIGHT);
        let mapped = Rect::new(
            target.work_area.left + rel_x,
            target.work_area.top + rel_y,
            target.work_area.left + rel_x + scaled_w,
            target.work_area.top + rel_y + scaled_h,
        );
        return Ok(clamp_rect_to_work_area(mapped, target.work_area));
    }

    let nearest_dst = nearest_monitor(rect.center(), dst_monitors);
    let scaled_w =
        scale_by_dpi(rect.width(), nearest_dst.dpi_x.max(96), src_dpi_x).max(MIN_FENCE_WIDTH);
    let scaled_h =
        scale_by_dpi(rect.height(), nearest_dst.dpi_y.max(96), src_dpi_y).max(MIN_FENCE_HEIGHT);
    let candidate = Rect::new(
        rect.left,
        rect.top,
        rect.left + scaled_w,
        rect.top + scaled_h,
    );
    clamp_rect_to_monitors(candidate, dst_monitors)
}

fn migrate_point_for_unseen_topology(
    pt: Point,
    icon_margin: (i32, i32),
    src_monitors: &[MonitorDescriptor],
    dst_monitors: &[MonitorDescriptor],
) -> Result<(Point, bool), TopologyError> {
    if dst_monitors.is_empty() {
        return Err(TopologyError::EmptyTransient);
    }
    let src_mon = nearest_monitor(pt, src_monitors);
    if let Some(target) = dst_monitors
        .iter()
        .find(|m| m.device_key == src_mon.device_key)
    {
        let rel_x = scale_by_dpi(
            pt.x - src_mon.work_area.left,
            target.dpi_x.max(96),
            src_mon.dpi_x.max(96),
        );
        let rel_y = scale_by_dpi(
            pt.y - src_mon.work_area.top,
            target.dpi_y.max(96),
            src_mon.dpi_y.max(96),
        );
        let mapped = Point {
            x: target.work_area.left + rel_x,
            y: target.work_area.top + rel_y,
        };
        return clamp_point_to_monitors(mapped, icon_margin, std::slice::from_ref(target));
    }
    clamp_point_to_monitors(pt, icon_margin, dst_monitors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(key: &str, l: i32, t: i32, r: i32, b: i32, primary: bool) -> MonitorDescriptor {
        MonitorDescriptor {
            device_key: key.into(),
            friendly_name: key.into(),
            rect: Rect::new(l, t, r, b),
            work_area: Rect::new(l, t, r, b - 48),
            dpi_x: 96,
            dpi_y: 96,
            is_primary: primary,
        }
    }

    #[test]
    fn topology_roundtrip_restores_and_clamps_single_monitor() {
        let dual = DisplayTopology::try_from_monitors(vec![
            monitor("DP-1", 0, 0, 2560, 1440, true),
            monitor("HDMI-1", 2560, 0, 4480, 1080, false),
        ])
        .unwrap();
        let single = DisplayTopology::try_from_monitors(vec![
            monitor("DP-1", 0, 0, 2560, 1440, true),
        ])
        .unwrap();

        let mut cfg = NativeLayoutConfig {
            fences: vec![FenceSpec {
                id: 101,
                name: "Work".into(),
                kind: FenceKind::IconGrid,
                rect: Rect::new(2800, 100, 3100, 400),
                collapsed: false,
                locked: false,
                role: None,
                portal_dir: None,
                backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
                layout_override: None,
                items: vec![],
            }],
            ..Default::default()
        };

        let r1 = cfg.apply_topology(&dual, (72, 72)).unwrap();
        assert_eq!(r1.clamped_fences, 0);
        assert_eq!(cfg.fences[0].rect, Rect::new(2800, 100, 3100, 400));

        // Disconnect 2nd monitor -> fence is clamped onto DP-1 without stretching size.
        let r2 = cfg.apply_topology(&single, (72, 72)).unwrap();
        assert_eq!(r2.clamped_fences, 1);
        assert_eq!(cfg.fences[0].rect.width(), 300);
        assert_eq!(cfg.fences[0].rect.height(), 300);
        assert_eq!(cfg.fences[0].rect.right, 2560);

        // Reconnect 2nd monitor -> exact dual-monitor layout restored from snapshot.
        let r3 = cfg.apply_topology(&dual, (72, 72)).unwrap();
        assert!(r3.restored_from_snapshot);
        assert_eq!(r3.clamped_fences, 0);
        assert_eq!(cfg.fences[0].rect, Rect::new(2800, 100, 3100, 400));
    }

    fn make_fence(id: i32, name: &str, role: Option<FenceRole>, portal_dir: Option<&str>) -> FenceSpec {
        FenceSpec {
            id,
            name: name.into(),
            kind: FenceKind::IconGrid,
            rect: Rect::new(100, 100, 400, 400),
            collapsed: false,
            locked: false,
            role,
            portal_dir: portal_dir.map(str::to_string),
            backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
            layout_override: None,
            items: vec![],
        }
    }

    fn make_item(name: &str, path: &str, order: i32) -> FenceItem {
        FenceItem {
            display_name: name.into(),
            order,
            kind: ItemKind::File,
            path: path.into(),
        }
    }

    #[test]
    fn role_resolution_and_portal_dir_backward_compatibility() {
        let desktops = vec![r"C:\Users\Alice\Desktop".to_string()];

        let mut desktop_portal_legacy =
            make_fence(101, "Desktop", None, Some(r"c:/Users/Alice/Desktop\"));
        desktop_portal_legacy
            .items
            .push(make_item("keep.txt", r"C:\Users\Alice\Desktop\keep.txt", 0));
        let orig_rect = desktop_portal_legacy.rect;
        assert_eq!(
            desktop_portal_legacy.effective_role(&desktops),
            FenceRole::DesktopBucket
        );
        assert!(desktop_portal_legacy.is_desktop_bucket(&desktops));
        assert!(!desktop_portal_legacy.is_unfiled_desktop(&desktops));
        desktop_portal_legacy.normalize_role(&desktops);
        assert_eq!(desktop_portal_legacy.role, Some(FenceRole::DesktopBucket));
        assert_eq!(desktop_portal_legacy.portal_dir, None);
        assert_eq!(desktop_portal_legacy.rect, orig_rect);
        assert_eq!(desktop_portal_legacy.items.len(), 1);

        let mut persisted_unfiled = make_fence(
            104,
            "Desktop",
            Some(FenceRole::UnfiledDesktop),
            Some(r"C:\Users\Alice\Desktop"),
        );
        persisted_unfiled
            .items
            .push(make_item("in_box.lnk", r"C:\Users\Alice\Desktop\in_box.lnk", 0));
        assert_eq!(
            persisted_unfiled.effective_role(&desktops),
            FenceRole::DesktopBucket
        );
        assert!(!persisted_unfiled.is_unfiled_desktop(&desktops));
        persisted_unfiled.normalize_role(&desktops);
        assert_eq!(persisted_unfiled.role, Some(FenceRole::DesktopBucket));
        assert_eq!(persisted_unfiled.portal_dir, None);
        assert_eq!(persisted_unfiled.items.len(), 1);

        let mut ext_legacy = make_fence(102, "Docs", None, Some(r"D:\Work\Docs"));
        assert_eq!(
            ext_legacy.effective_role(&desktops),
            FenceRole::ExternalPortal {
                dir: r"D:\Work\Docs".into()
            }
        );
        ext_legacy.normalize_role(&desktops);
        assert_eq!(
            ext_legacy.portal_dir.as_deref(),
            Some(r"D:\Work\Docs")
        );

        let mut bucket = make_fence(103, "Tools", None, None);
        assert_eq!(bucket.effective_role(&desktops), FenceRole::DesktopBucket);
        bucket.normalize_role(&desktops);
        assert_eq!(bucket.role, Some(FenceRole::DesktopBucket));
    }

    #[test]
    fn desktop_bucket_ownership_and_free_icon_reconciliation() {
        let desktops = vec![r"C:\Users\Alice\Desktop".to_string()];
        let topo = DisplayTopology::try_from_monitors(vec![
            monitor("DP-1", 0, 0, 1920, 1080, true),
        ])
        .unwrap();

        let mut b1 = make_fence(101, "Work", Some(FenceRole::DesktopBucket), None);
        b1.items.push(make_item("a.txt", r"C:\Users\Alice\Desktop\a.txt", 0));
        let b2 = make_fence(102, "Play", Some(FenceRole::DesktopBucket), None);
        // Legacy UnfiledDesktop fence now behaves identically to a normal DesktopBucket:
        // deduplicated under single ownership and never swallows FreeDesktop.
        let mut legacy_desktop_fence =
            make_fence(104, "Desktop", Some(FenceRole::UnfiledDesktop), None);
        legacy_desktop_fence
            .items
            .push(make_item("a.txt", r"C:\Users\Alice\Desktop\a.txt", 0));

        let mut cfg = NativeLayoutConfig {
            fences: vec![b1, b2, legacy_desktop_fence],
            ..Default::default()
        };
        cfg.apply_topology(&topo, (84, 92)).unwrap();
        assert_eq!(cfg.unfiled_desktop_fence_id(&desktops), None);

        assert!(cfg.upsert_free_icon(
            DesktopIconPos {
                id: r"C:\Users\Alice\Desktop\free.lnk".into(),
                display_name: "free.lnk".into(),
                pos: Point { x: 40, y: 40 },
            },
            &desktops
        ));
        // Duplicate between b1 (101) and legacy_desktop_fence (104) is purged, while
        // unowned free.lnk remains untouched in free_icons.
        assert_eq!(cfg.enforce_desktop_single_ownership(&desktops), 1);
        assert!(cfg.fences[2].items.is_empty());
        assert_eq!(cfg.active_free_icons().len(), 1);

        // Bucket-owned item cannot be added to free_icons.
        assert!(!cfg.upsert_free_icon(
            DesktopIconPos {
                id: r"c:\users\alice\desktop\A.TXT".into(),
                display_name: "a.txt".into(),
                pos: Point { x: 124, y: 40 },
            },
            &desktops
        ));

        // Move a.txt and free.lnk into b2 -> removed from b1 and free_icons atomically.
        assert!(cfg.assign_desktop_items_to_bucket(
            102,
            vec![
                make_item("a.txt", r"c:/Users/Alice/Desktop/A.TXT", 0),
                make_item("free.lnk", r"C:\Users\Alice\Desktop\free.lnk", 1),
            ],
            &desktops
        ));
        assert!(cfg.fences[0].items.is_empty());
        assert_eq!(cfg.fences[1].items.len(), 2);
        assert_eq!(cfg.desktop_owner_of(r"C:\Users\Alice\Desktop\a.txt", &desktops), Some(102));
        assert!(cfg.active_free_icons().is_empty());
        // Re-check reconcile adds newly discovered unfiled item without touching b2 items.
        let scanned = vec![
            make_item("a.txt", r"C:\Users\Alice\Desktop\a.txt", 0),
            make_item("free.lnk", r"C:\Users\Alice\Desktop\free.lnk", 1),
            make_item("new.pdf", r"C:\Users\Alice\Desktop\new.pdf", 2),
        ];
        let changes = cfg
            .reconcile_free_icons(&scanned, Point { x: 24, y: 24 }, (84, 92), topo.monitors(), &desktops)
            .unwrap();
        assert_eq!(changes, 1);
        assert_eq!(cfg.active_free_icons().len(), 1);
        assert_eq!(cfg.active_free_icons()[0].display_name, "new.pdf");

        // Release free.lnk back from b2 (and from normalized legacy_desktop_fence if assigned)
        // and verify it returns to free_icons instead of being absorbed.
        assert_eq!(
            cfg.release_desktop_items(102, &[r"C:\Users\Alice\Desktop\free.lnk".into()], &desktops),
            1
        );
        assert_eq!(cfg.fences[1].items.len(), 1);
        assert!(cfg.remove_desktop_path_everywhere(r"c:\users\alice\desktop\a.txt", &desktops));
        assert!(cfg.fences[1].items.is_empty());
        // Reconcile now places freed free.lnk + existing new.pdf without overlapping slots.
        let changes2 = cfg
            .reconcile_free_icons(&scanned[1..], Point { x: 24, y: 24 }, (84, 92), topo.monitors(), &desktops)
            .unwrap();
        assert_eq!(changes2, 1);
        assert_eq!(cfg.active_free_icons().len(), 2);
        assert_ne!(cfg.active_free_icons()[0].pos, cfg.active_free_icons()[1].pos);
    }

    #[test]
    fn unseen_topology_migrates_by_device_key_and_dpi() {
        let m1_100 = monitor("DP-1", 0, 0, 2560, 1440, true);
        let m2_100 = monitor("HDMI-1", 2560, 0, 4480, 1080, false);
        let topo_right =
            DisplayTopology::try_from_monitors(vec![m1_100.clone(), m2_100]).unwrap();

        let mut m2_left_150 = monitor("HDMI-1", -2880, 0, 0, 1620, false);
        m2_left_150.dpi_x = 144;
        m2_left_150.dpi_y = 144;
        let topo_left_hidpi =
            DisplayTopology::try_from_monitors(vec![m1_100, m2_left_150]).unwrap();

        let mut cfg = NativeLayoutConfig {
            fences: vec![make_fence(101, "Side", Some(FenceRole::DesktopBucket), None)],
            ..Default::default()
        };
        cfg.fences[0].rect = Rect::new(2760, 100, 3060, 400); // rel (200, 100), 300x300 on HDMI-1
        cfg.apply_topology(&topo_right, (84, 92)).unwrap();
        cfg.upsert_free_icon(
            DesktopIconPos {
                id: r"C:\Users\Alice\Desktop\side.lnk".into(),
                display_name: "side.lnk".into(),
                pos: Point { x: 2660, y: 200 }, // rel (100, 200) on HDMI-1
            },
            &[],
        );

        let report = cfg.apply_topology(&topo_left_hidpi, (84, 92)).unwrap();
        assert!(!report.restored_from_snapshot);
        assert_eq!(report.clamped_fences, 0);
        // Moved onto left-side HDMI-1 and scaled by 144/96 = 1.5x.
        assert_eq!(cfg.fences[0].rect, Rect::new(-2580, 150, -2130, 600));
        assert_eq!(cfg.active_free_icons().len(), 1);
        assert_eq!(cfg.active_free_icons()[0].pos, Point { x: -2730, y: 300 });
    }

    #[test]
    fn deserializes_m1_json_without_m3_fields() {
        let m1_json = r#"{
            "schema_version": 1,
            "active_topology": null,
            "fences": [{
                "id": 101,
                "name": "Legacy",
                "kind": "icon_grid",
                "rect": { "left": 10, "top": 20, "right": 310, "bottom": 320 },
                "items": []
            }],
            "snapshots": {}
        }"#;
        let parsed: NativeLayoutConfig = serde_json::from_str(m1_json).unwrap();
        assert_eq!(parsed.fences.len(), 1);
        assert!(!parsed.fences[0].collapsed);
        assert!(!parsed.hide_all_fences);
        assert_eq!(parsed.fences[0].role, None);
        assert_eq!(parsed.fences[0].portal_dir, None);
        assert_eq!(parsed.fences[0].backdrop_alpha, DEFAULT_BACKDROP_ALPHA);
        assert_eq!(parsed.preferences, LayoutPreferences::default());
        assert_eq!(parsed.next_fence_id(), 102);
    }
}
