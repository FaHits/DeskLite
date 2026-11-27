pub mod autostart;
pub mod desktop_listview;
pub mod display;
pub mod dpi;
pub mod drag_ghost;
pub mod drag_out;
pub mod events;
pub mod fence_poc;
pub mod folder_watch;
pub mod free_desktop_wnd;
pub mod icons;
pub mod shell_menu;
pub mod shell_ops;
pub mod tray;
pub mod window_attach;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Win32Error {
    #[error("topology error: {0}")]
    Topology(#[from] desklite_core::TopologyError),
    #[error("Windows API error in {op}: {source}")]
    Win {
        op: &'static str,
        #[source]
        source: windows::core::Error,
    },
    #[error("Win32 status error in {op}: code {code}")]
    Status { op: &'static str, code: u32 },
    #[error("desktop SysListView32 window not found")]
    DesktopListViewNotFound,
    #[error("remote allocation in explorer.exe failed")]
    RemoteAllocFailed,
    #[error("icon index {0} out of range (count: {1})")]
    IconIndexOutOfRange(usize, usize),
}

pub(crate) fn win_err(op: &'static str, source: windows::core::Error) -> Win32Error {
    Win32Error::Win { op, source }
}

pub use autostart::{
    AutostartMode, AutostartSetOutcome, AutostartStatus, handle_autostart_cli_args,
    query_autostart_status, set_autostart_mode,
};
pub use desktop_listview::{
    DesktopIconInfo, DesktopListView, find_desktop_listview, hide_desktop_icons_for_paths,
    hide_desktop_icons_for_paths_async, hide_desktop_icons_for_paths_delayed,
    is_shell_desktop_visible, probe_desktop_window_tree, restore_all_desktop_icons,
    restore_desktop_icon, restore_desktop_icon_async, restore_desktop_icons_async,
    set_shell_desktop_icons_visible,
};
pub use display::probe_display_topology;
pub use dpi::enable_per_monitor_dpi_v2;
pub use drag_ghost::DragGhost;
pub use drag_out::{
    AsyncDragRequest, DragOutOutcome, DragTargetOutcome, clipboard_has_files,
    copy_files_to_clipboard, copy_text_to_clipboard, read_clipboard_files, read_clipboard_text,
    set_clipboard_files, set_clipboard_files_for_window, start_file_drag_out,
    start_file_drag_out_async,
};
pub use events::{
    DisplayEventKind, PowerSessionNotificationGuard, is_display_wake_power_broadcast,
    register_power_and_session_notifications, run_event_monitor,
    unregister_power_and_session_notifications,
};
pub use fence_poc::{FencePocConfig, run_bottom_fence_poc};
pub use folder_watch::{FolderWatcher, WM_APP_PORTAL_DIRTY};
pub use free_desktop_wnd::{
    FreeDesktopController, FreeDesktopEvent, FreeDesktopHotkey, FreeDesktopNavKey,
    FreeDesktopRenderItem, FreeDesktopSnapshot,
};
pub use icons::{DecodedIcon, IconCache, extract_file_icon_pbgra};
pub use shell_menu::{
    ActiveShellMenuGuard, CUSTOM_MENU_CMD_BASE, CustomMenuItem, HybridMenuOutcome,
    SHELL_MENU_CMD_FIRST, SHELL_MENU_CMD_LAST, ShellVerbInterception, append_custom_items,
    append_lazy_desktop_background_submenu, append_menu_separator, classify_shell_verb,
    clear_prefetched_shell_menu, handle_active_shell_menu_msg, invoke_shell_context_command,
    is_shell_menu_cmd_id, paths_share_same_shell_parent, prefetch_shell_items_menu,
    show_hybrid_shell_items_menu, warmup_shell_context_menu,
};
pub use shell_ops::{
    all_desktop_dirs, delete_files_permanently, is_desktop_dir_path, is_directly_on_desktop,
    is_vault_path, launch_as_admin, launch_item, migrate_vault_item_to_desktop, move_path_in_place,
    paste_files_to_directory, paste_files_to_directory_detailed, pick_folder_dialog,
    rename_file_in_place, resolve_dropped_item, reveal_in_explorer, scan_portal_directory,
    show_file_properties, trash_files, user_desktop_dir,
};
pub use tray::{TrayIcon, WM_APP_TRAY};
pub use window_attach::{
    DesktopZHookGuard, UiThreadCloakGuard, Win32Hotkey, all_bucket_fence_hwnds, all_fence_hwnds,
    allow_foreground_self, attach_desktop_owner_and_rounded_corners, attach_free_desktop_canvas,
    cursor_screen_pos, enable_dark_popup_menus, find_window_by_exact_title, focus_window_now,
    force_foreground_for_popup, foreground_window_handle, free_desktop_hwnd,
    hide_all_fences_immediately, inspector_hwnd, install_desktop_z_win_event_hooks,
    install_ui_thread_cloak_hook, invalidate_window_no_erase, is_ctrl_key_down,
    is_file_drag_active, is_free_desktop_hwnd, is_in_size_move, is_popup_menu_hwnd,
    is_primary_mouse_down, live_sizemove_rect_for_hwnd, move_window_no_z,
    post_reassert_all_fences_z, post_uncloak_after_swap, post_window_message,
    promote_fence_above_siblings, reassert_desktop_z_order_sync, set_active_text_edit_hwnd,
    set_external_foreground_notify_target, set_file_drag_active, set_hwnd_locked,
    set_inspector_hwnd, set_window_cloaked, set_window_rect_no_z, set_z_reassert_notify_target,
    strip_alt_tab_from_host, style_floating_inspector_window, take_win32_hotkeys,
    window_outer_rect,
};
