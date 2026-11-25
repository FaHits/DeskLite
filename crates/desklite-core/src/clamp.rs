use crate::topology::{MonitorDescriptor, Point, Rect, TopologyError};

pub const MIN_FENCE_WIDTH: i32 = 120;
pub const MIN_FENCE_HEIGHT: i32 = 80;

/// Picks the monitor whose work area is closest to `pt` using squared Euclidean distance.
pub fn nearest_monitor(pt: Point, monitors: &[MonitorDescriptor]) -> &MonitorDescriptor {
    debug_assert!(!monitors.is_empty());
    monitors
        .iter()
        .min_by_key(|m| {
            let w = m.work_area;
            let cx = i64::from(pt.x.clamp(w.left, w.right));
            let cy = i64::from(pt.y.clamp(w.top, w.bottom));
            let dx = i64::from(pt.x) - cx;
            let dy = i64::from(pt.y) - cy;
            dx * dx + dy * dy
        })
        .expect("monitors must be non-empty")
}

/// Translate-first clamping into `work`:
/// preserves box width/height whenever possible, only shrinking if the box exceeds `work`.
pub fn clamp_rect_to_work_area(rect: Rect, work: Rect) -> (Rect, bool) {
    let work_w = work.width().max(1);
    let work_h = work.height().max(1);
    let min_w = MIN_FENCE_WIDTH.min(work_w);
    let min_h = MIN_FENCE_HEIGHT.min(work_h);

    let orig_w = rect.width();
    let orig_h = rect.height();
    let target_w = orig_w.clamp(min_w, work_w);
    let target_h = orig_h.clamp(min_h, work_h);

    let max_left = (work.right - target_w).max(work.left);
    let max_top = (work.bottom - target_h).max(work.top);

    let left = rect.left.clamp(work.left, max_left);
    let top = rect.top.clamp(work.top, max_top);
    let out = Rect::new(left, top, left + target_w, top + target_h);
    (out, out != rect)
}

pub fn clamp_rect_to_monitors(
    rect: Rect,
    monitors: &[MonitorDescriptor],
) -> Result<(Rect, bool), TopologyError> {
    if monitors.is_empty() {
        return Err(TopologyError::EmptyTransient);
    }
    let target = nearest_monitor(rect.center(), monitors);
    Ok(clamp_rect_to_work_area(rect, target.work_area))
}

pub fn clamp_point_to_monitors(
    pt: Point,
    icon_margin: (i32, i32),
    monitors: &[MonitorDescriptor],
) -> Result<(Point, bool), TopologyError> {
    if monitors.is_empty() {
        return Err(TopologyError::EmptyTransient);
    }
    let target = nearest_monitor(pt, monitors);
    let w = target.work_area;
    let max_x = (w.right - icon_margin.0.max(0)).max(w.left);
    let max_y = (w.bottom - icon_margin.1.max(0)).max(w.top);
    let out = Point {
        x: pt.x.clamp(w.left, max_x),
        y: pt.y.clamp(w.top, max_y),
    };
    Ok((out, out != pt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary_2k() -> MonitorDescriptor {
        MonitorDescriptor {
            device_key: "DP-1".into(),
            friendly_name: "Primary".into(),
            rect: Rect::new(0, 0, 2560, 1440),
            work_area: Rect::new(0, 0, 2560, 1392),
            dpi_x: 96,
            dpi_y: 96,
            is_primary: true,
        }
    }

    #[test]
    fn preserves_size_and_translates_into_work_area() {
        let mon = primary_2k();
        // Box sits completely off-screen to the right (e.g. disconnected 2nd monitor at x=3000)
        let old = Rect::new(3000, 200, 3264, 428);
        let (clamped, changed) = clamp_rect_to_monitors(old, &[mon]).unwrap();
        assert!(changed);
        assert_eq!(clamped.width(), 264);
        assert_eq!(clamped.height(), 228);
        assert_eq!(clamped, Rect::new(2296, 200, 2560, 428));
    }

    #[test]
    fn shrinks_only_when_larger_than_target_work_area() {
        let small = MonitorDescriptor {
            device_key: "LAPTOP".into(),
            friendly_name: "Laptop".into(),
            rect: Rect::new(0, 0, 1280, 720),
            work_area: Rect::new(0, 0, 1280, 680),
            dpi_x: 96,
            dpi_y: 96,
            is_primary: true,
        };
        let huge = Rect::new(100, 100, 1700, 1000);
        let (clamped, changed) = clamp_rect_to_monitors(huge, &[small]).unwrap();
        assert!(changed);
        assert_eq!(clamped, Rect::new(0, 0, 1280, 680));
    }

    #[test]
    fn single_monitor_in_bounds_stays_untouched() {
        let mon = primary_2k();
        let ok = Rect::new(2286, 10, 2550, 238);
        let (clamped, changed) = clamp_rect_to_monitors(ok, &[mon]).unwrap();
        assert!(!changed);
        assert_eq!(clamped, ok);
    }

    #[test]
    fn point_clamps_to_nearest_monitor_work_area() {
        let mon = primary_2k();
        let (pt, changed) = clamp_point_to_monitors(Point { x: 2700, y: 1420 }, (72, 72), &[mon]).unwrap();
        assert!(changed);
        assert_eq!(pt, Point { x: 2488, y: 1320 });
    }
}
