use desklite_core::{MonitorDescriptor, Rect, nearest_monitor};

/// Snaps a dragged fence rectangle to monitor work-area edges and sibling fence edges
/// within `threshold_px` (bypassed when `alt_held` is true).
pub fn snap_move_rect(
    rect: Rect,
    siblings: &[Rect],
    monitors: &[MonitorDescriptor],
    threshold_px: i32,
    alt_held: bool,
) -> Rect {
    if monitors.is_empty() {
        return rect;
    }
    let w = rect.width().max(1);
    let h = rect.height().max(1);
    let mon = nearest_monitor(rect.center(), monitors);
    let work = mon.work_area;

    let mut left = rect.left;
    let mut top = rect.top;

    if !alt_held && threshold_px > 0 {
        let mut x_targets = vec![work.left, work.right - w];
        let mut y_targets = vec![work.top, work.bottom - h];

        for sib in siblings {
            // Vertical overlap or proximity encourages horizontal alignment
            x_targets.push(sib.left);
            x_targets.push(sib.right);
            x_targets.push(sib.left - w);
            x_targets.push(sib.right - w);

            y_targets.push(sib.top);
            y_targets.push(sib.bottom);
            y_targets.push(sib.top - h);
            y_targets.push(sib.bottom - h);
        }

        if let Some(best_x) = x_targets
            .into_iter()
            .filter(|&t| (t - left).abs() <= threshold_px)
            .min_by_key(|&t| (t - left).abs())
        {
            left = best_x;
        }

        if let Some(best_y) = y_targets
            .into_iter()
            .filter(|&t| (t - top).abs() <= threshold_px)
            .min_by_key(|&t| (t - top).abs())
        {
            top = best_y;
        }
    }

    let max_left = (work.right - w).max(work.left);
    let max_top = (work.bottom - h).max(work.top);
    let clamped_left = left.clamp(work.left, max_left);
    let clamped_top = top.clamp(work.top, max_top);
    Rect::new(clamped_left, clamped_top, clamped_left + w, clamped_top + h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon_2k() -> MonitorDescriptor {
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
    fn snaps_to_work_area_and_sibling_edges_unless_alt_held() {
        let mon = mon_2k();
        let sib = Rect::new(2200, 10, 2464, 238);
        let dragged = Rect::new(2206, 245, 2470, 473);

        let snapped = snap_move_rect(dragged, &[sib], std::slice::from_ref(&mon), 12, false);
        assert_eq!(snapped, Rect::new(2200, 238, 2464, 466));

        let near_edge = Rect::new(2292, 20, 2556, 248);
        let snapped_edge = snap_move_rect(near_edge, &[], std::slice::from_ref(&mon), 12, false);
        assert_eq!(snapped_edge, Rect::new(2296, 20, 2560, 248));

        let unsnapped = snap_move_rect(dragged, &[sib], &[mon], 12, true);
        assert_eq!(unsnapped, dragged);
    }
}
