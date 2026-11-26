pub mod actions;
pub mod fence_view;
pub mod fonts;
pub mod free_desktop_view;
pub mod hit_test;
pub mod inspector;
pub mod item_grid;
pub mod snap;
pub mod state;
pub mod undo;

pub use fence_view::{
    COLLAPSED_HEIGHT, CollapsedMenuCommand, FREE_DESKTOP_TITLE, FenceCommand, FenceUiOutput,
    FreeDesktopMenuCommand, FreeIconMenuCommand, INSPECTOR_HEIGHT, INSPECTOR_TITLE,
    INSPECTOR_WIDTH, SharedFenceState, apply_collapsed_menu_command,
    apply_free_desktop_menu_command, apply_free_icon_menu_command, configure_egui_theme,
    render_fence_viewport, render_free_desktop_viewport, render_inspector_viewport,
};
pub use fonts::install_chinese_fonts;
pub use snap::snap_move_rect;
