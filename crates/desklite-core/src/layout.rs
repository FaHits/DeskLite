use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::clamp::{clamp_point_to_monitors, clamp_rect_to_monitors};
use crate::topology::{DisplayTopology, MonitorDescriptor, Point, Rect, TopologyError, TopologyKey};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FenceSpec {
    pub id: i32,
    pub name: String,
    pub kind: FenceKind,
    pub rect: Rect,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portal_dir: Option<String>,
    #[serde(default = "default_backdrop_alpha")]
    pub backdrop_alpha: u8,
    pub items: Vec<FenceItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutPreferences {
    #[serde(default = "default_cell_w")]
    pub icon_cell_w: i32,
    #[serde(default = "default_cell_h")]
    pub icon_cell_h: i32,
    #[serde(default = "default_snap_px")]
    pub snap_threshold_px: i32,
}

impl Default for LayoutPreferences {
    fn default() -> Self {
        Self {
            icon_cell_w: default_cell_w(),
            icon_cell_h: default_cell_h(),
            snap_threshold_px: default_snap_px(),
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
    /// Applies a verified non-empty topology:
    /// - Restores per-topology fence coordinates if previously seen, otherwise uses current rects.
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

        let mut clamped_fences = 0usize;
        let mut fence_rects = BTreeMap::new();
        for fence in &mut self.fences {
            let candidate = existing_snapshot
                .as_ref()
                .and_then(|snap| snap.fences.get(&fence.id).copied())
                .unwrap_or(fence.rect);
            let (clamped, changed) = clamp_rect_to_monitors(candidate, monitors)?;
            if changed {
                clamped_fences += 1;
            }
            fence.rect = clamped;
            fence_rects.insert(fence.id, clamped);
        }

        let mut clamped_icons = 0usize;
        let mut free_icons = existing_snapshot
            .map(|snap| snap.free_icons)
            .unwrap_or_default();
        for icon in &mut free_icons {
            let (clamped, changed) = clamp_point_to_monitors(icon.pos, icon_margin, monitors)?;
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
                portal_dir: None,
                backdrop_alpha: DEFAULT_BACKDROP_ALPHA,
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
        assert_eq!(parsed.fences[0].portal_dir, None);
        assert_eq!(parsed.fences[0].backdrop_alpha, DEFAULT_BACKDROP_ALPHA);
        assert_eq!(parsed.preferences, LayoutPreferences::default());
        assert_eq!(parsed.next_fence_id(), 102);
    }
}
