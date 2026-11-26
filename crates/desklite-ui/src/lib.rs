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
    FreeDesktopDispatchResult, FreeDesktopMenuCommand, FreeIconMenuCommand, INSPECTOR_HEIGHT,
    INSPECTOR_TITLE, INSPECTOR_WIDTH, SelectionScope, SharedFenceState,
    apply_collapsed_menu_command, apply_free_desktop_menu_command, apply_free_icon_menu_command,
    build_free_desktop_snapshot, configure_egui_theme, handle_free_desktop_event,
    render_fence_viewport, render_inspector_viewport,
};
pub use free_desktop_view::{free_icon_screen_rect, handle_free_desktop_drop, pending_free_rename};
pub use fonts::install_chinese_fonts;
pub use snap::snap_move_rect;
