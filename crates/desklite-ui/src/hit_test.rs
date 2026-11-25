use desklite_core::FenceKind;

pub const TITLE_BAR_HEIGHT: i32 = 34;
pub const RESIZE_BORDER: i32 = 8;
pub const LIST_ROW_HEIGHT: i32 = 30;
pub const BODY_PADDING_X: i32 = 8;
pub const BODY_PADDING_Y: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitRegion {
    Resize(ResizeEdge),
    TitleBar,
    Item(usize),
    EmptyBody,
}

pub fn grid_columns(width: i32, cell_w: i32) -> usize {
    let inner_w = (width - BODY_PADDING_X * 2).max(cell_w.max(1));
    (inner_w / cell_w.max(1)).max(1) as usize
}

pub fn content_height(
    kind: FenceKind,
    width: i32,
    item_count: usize,
    cell_w: i32,
    cell_h: i32,
) -> i32 {
    if item_count == 0 {
        return 0;
    }
    match kind {
        FenceKind::FileList => (item_count as i32) * LIST_ROW_HEIGHT + BODY_PADDING_Y * 2,
        FenceKind::IconGrid | FenceKind::Unknown(_) => {
            let cols = grid_columns(width, cell_w);
            let rows = item_count.div_ceil(cols);
            (rows as i32) * cell_h.max(1) + BODY_PADDING_Y * 2
        }
    }
}

pub fn max_scroll_offset(content_h: i32, window_h: i32) -> i32 {
    let body_h = (window_h - TITLE_BAR_HEIGHT).max(0);
    (content_h - body_h).max(0)
}

pub fn item_rect(
    index: usize,
    kind: FenceKind,
    width: i32,
    cell_w: i32,
    cell_h: i32,
    scroll_y: i32,
) -> (i32, i32, i32, i32) {
    match kind {
        FenceKind::FileList => {
            let left = BODY_PADDING_X;
            let right = (width - BODY_PADDING_X).max(left + 16);
            let top = TITLE_BAR_HEIGHT + BODY_PADDING_Y + (index as i32) * LIST_ROW_HEIGHT - scroll_y;
            (left, top, right, top + LIST_ROW_HEIGHT)
        }
        FenceKind::IconGrid | FenceKind::Unknown(_) => {
            let cols = grid_columns(width, cell_w);
            let row = (index / cols) as i32;
            let col = (index % cols) as i32;
            let total_grid_w = (cols as i32) * cell_w.max(1);
            let extra = ((width - BODY_PADDING_X * 2) - total_grid_w).max(0);
            let left_pad = BODY_PADDING_X + extra / 2;
            let left = left_pad + col * cell_w.max(1);
            let top = TITLE_BAR_HEIGHT + BODY_PADDING_Y + row * cell_h.max(1) - scroll_y;
            (left, top, left + cell_w.max(1), top + cell_h.max(1))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn hit_test_point(
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    collapsed: bool,
    kind: FenceKind,
    item_count: usize,
    cell_w: i32,
    cell_h: i32,
    scroll_y: i32,
) -> HitRegion {
    let on_left = x < RESIZE_BORDER;
    let on_right = x >= width - RESIZE_BORDER;
    let on_top = y < RESIZE_BORDER;
    let on_bottom = y >= height - RESIZE_BORDER;

    if collapsed {
        if on_left {
            return HitRegion::Resize(ResizeEdge::Left);
        }
        if on_right {
            return HitRegion::Resize(ResizeEdge::Right);
        }
        return HitRegion::TitleBar;
    }

    match (on_left, on_right, on_top, on_bottom) {
        (true, _, true, _) => return HitRegion::Resize(ResizeEdge::TopLeft),
        (_, true, true, _) => return HitRegion::Resize(ResizeEdge::TopRight),
        (true, _, _, true) => return HitRegion::Resize(ResizeEdge::BottomLeft),
        (_, true, _, true) => return HitRegion::Resize(ResizeEdge::BottomRight),
        (true, _, _, _) => return HitRegion::Resize(ResizeEdge::Left),
        (_, true, _, _) => return HitRegion::Resize(ResizeEdge::Right),
        (_, _, true, _) => return HitRegion::Resize(ResizeEdge::Top),
        (_, _, _, true) => return HitRegion::Resize(ResizeEdge::Bottom),
        _ => {}
    }

    if y < TITLE_BAR_HEIGHT {
        return HitRegion::TitleBar;
    }

    for i in 0..item_count {
        let (l, t, r, b) = item_rect(i, kind, width, cell_w, cell_h, scroll_y);
        if x >= l && x < r && y >= t && y < b {
            return HitRegion::Item(i);
        }
    }

    HitRegion::EmptyBody
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_detects_edges_title_and_grid_items() {
        assert_eq!(
            hit_test_point(300, 240, 2, 2, false, FenceKind::IconGrid, 4, 84, 92, 0),
            HitRegion::Resize(ResizeEdge::TopLeft)
        );
        assert_eq!(
            hit_test_point(300, 240, 150, 20, false, FenceKind::IconGrid, 4, 84, 92, 0),
            HitRegion::TitleBar
        );
        assert_eq!(
            hit_test_point(300, 240, 50, 60, false, FenceKind::IconGrid, 4, 84, 92, 0),
            HitRegion::Item(0)
        );
        // Collapsed allows only horizontal resize on edges
        assert_eq!(
            hit_test_point(300, 34, 150, 2, true, FenceKind::IconGrid, 4, 84, 92, 0),
            HitRegion::TitleBar
        );
    }

    #[test]
    fn scroll_bounds_account_for_title_bar() {
        let ch = content_height(FenceKind::FileList, 240, 10, 84, 92);
        assert_eq!(ch, 10 * LIST_ROW_HEIGHT + BODY_PADDING_Y * 2);
        assert_eq!(max_scroll_offset(ch, 200), ch - (200 - TITLE_BAR_HEIGHT));
        assert_eq!(max_scroll_offset(50, 200), 0);
    }
}
