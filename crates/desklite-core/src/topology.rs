use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    pub const fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    pub const fn center(self) -> Point {
        Point {
            x: self.left + self.width() / 2,
            y: self.top + self.height() / 2,
        }
    }

    pub const fn is_valid(self) -> bool {
        self.width() > 0 && self.height() > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorDescriptor {
    pub device_key: String,
    pub friendly_name: String,
    pub rect: Rect,
    pub work_area: Rect,
    pub dpi_x: u32,
    pub dpi_y: u32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TopologyKey(pub String);

impl std::fmt::Display for TopologyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Error)]
pub enum TopologyError {
    #[error("transient zero-monitor state ignored to preserve baseline topology")]
    EmptyTransient,
    #[error("monitor descriptor has non-positive work area: {0:?}")]
    InvalidMonitor(Rect),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayTopology {
    monitors: Vec<MonitorDescriptor>,
    key: TopologyKey,
}

impl DisplayTopology {
    pub fn try_from_monitors(mut monitors: Vec<MonitorDescriptor>) -> Result<Self, TopologyError> {
        if monitors.is_empty() {
            return Err(TopologyError::EmptyTransient);
        }
        for m in &monitors {
            if !m.work_area.is_valid() {
                return Err(TopologyError::InvalidMonitor(m.work_area));
            }
        }
        monitors.sort_by(|a, b| {
            (!a.is_primary)
                .cmp(&(!b.is_primary))
                .then_with(|| a.rect.left.cmp(&b.rect.left))
                .then_with(|| a.rect.top.cmp(&b.rect.top))
                .then_with(|| a.device_key.cmp(&b.device_key))
        });
        let key = compute_topology_key(&monitors);
        Ok(Self { monitors, key })
    }

    pub fn monitors(&self) -> &[MonitorDescriptor] {
        &self.monitors
    }

    pub fn key(&self) -> &TopologyKey {
        &self.key
    }

    pub fn primary(&self) -> &MonitorDescriptor {
        self.monitors
            .iter()
            .find(|m| m.is_primary)
            .unwrap_or(&self.monitors[0])
    }

    pub fn virtual_bounds(&self) -> Rect {
        let mut left = i32::MAX;
        let mut top = i32::MAX;
        let mut right = i32::MIN;
        let mut bottom = i32::MIN;
        for m in &self.monitors {
            left = left.min(m.rect.left);
            top = top.min(m.rect.top);
            right = right.max(m.rect.right);
            bottom = bottom.max(m.rect.bottom);
        }
        Rect::new(left, top, right, bottom)
    }
}

fn fnv1a_update(mut hash: u64, bytes: &[u8]) -> u64 {
    const PRIME: u64 = 0x100000001b3;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn fnv1a_i32(hash: u64, v: i32) -> u64 {
    fnv1a_update(hash, &v.to_le_bytes())
}

fn fnv1a_u32(hash: u64, v: u32) -> u64 {
    fnv1a_update(hash, &v.to_le_bytes())
}

fn compute_topology_key(monitors: &[MonitorDescriptor]) -> TopologyKey {
    let mut h: u64 = 0xcbf29ce484222325;
    h = fnv1a_u32(h, monitors.len() as u32);
    for m in monitors {
        h = fnv1a_update(h, m.device_key.as_bytes());
        h = fnv1a_update(h, &[0xff]);
        for v in [
            m.rect.left,
            m.rect.top,
            m.rect.right,
            m.rect.bottom,
        ] {
            h = fnv1a_i32(h, v);
        }
        h = fnv1a_u32(h, m.dpi_x);
        h = fnv1a_u32(h, m.dpi_y);
        h = fnv1a_update(h, &[u8::from(m.is_primary)]);
    }
    let primary = monitors
        .iter()
        .find(|m| m.is_primary)
        .unwrap_or(&monitors[0]);
    TopologyKey(format!(
        "{}m_{}x{}_{:016x}",
        monitors.len(),
        primary.rect.width(),
        primary.rect.height(),
        h
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(key: &str, l: i32, t: i32, r: i32, b: i32, dpi: u32, primary: bool) -> MonitorDescriptor {
        MonitorDescriptor {
            device_key: key.to_string(),
            friendly_name: key.to_string(),
            rect: Rect::new(l, t, r, b),
            work_area: Rect::new(l, t, r, b - 48),
            dpi_x: dpi,
            dpi_y: dpi,
            is_primary: primary,
        }
    }

    #[test]
    fn rejects_empty_transient_topology() {
        let res = DisplayTopology::try_from_monitors(vec![]);
        assert!(matches!(res, Err(TopologyError::EmptyTransient)));
    }

    #[test]
    fn key_distinguishes_primary_swap_arrangement_and_dpi() {
        let a = DisplayTopology::try_from_monitors(vec![
            mon("DP-1", 0, 0, 2560, 1440, 96, true),
            mon("HDMI-1", 2560, 0, 4480, 1080, 96, false),
        ])
        .unwrap();

        let b_swapped_primary = DisplayTopology::try_from_monitors(vec![
            mon("DP-1", 0, 0, 2560, 1440, 96, false),
            mon("HDMI-1", 2560, 0, 4480, 1080, 96, true),
        ])
        .unwrap();

        let c_dpi_changed = DisplayTopology::try_from_monitors(vec![
            mon("DP-1", 0, 0, 2560, 1440, 144, true),
            mon("HDMI-1", 2560, 0, 4480, 1080, 96, false),
        ])
        .unwrap();

        let d_left_right_flipped = DisplayTopology::try_from_monitors(vec![
            mon("DP-1", 0, 0, 2560, 1440, 96, true),
            mon("HDMI-1", -1920, 0, 0, 1080, 96, false),
        ])
        .unwrap();

        assert_ne!(a.key(), b_swapped_primary.key());
        assert_ne!(a.key(), c_dpi_changed.key());
        assert_ne!(a.key(), d_left_right_flipped.key());
    }

    #[test]
    fn key_ignores_work_area_taskbar_changes() {
        let mut base = mon("DP-1", 0, 0, 2560, 1440, 96, true);
        let t1 = DisplayTopology::try_from_monitors(vec![base.clone()]).unwrap();
        // Simulate taskbar auto-hide or resizing changing only work_area.
        base.work_area = Rect::new(0, 0, 2560, 1440);
        let t2 = DisplayTopology::try_from_monitors(vec![base]).unwrap();
        assert_eq!(t1.key(), t2.key());
    }
}
