pub mod desktop_listview;
pub mod display;
pub mod dpi;
pub mod drag_out;
pub mod events;
pub mod fence_poc;
pub mod folder_watch;
pub mod icons;
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

pub use desktop_listview::{
    DesktopIconInfo, DesktopListView, find_desktop_listview, hide_desktop_icons_for_paths,
    probe_desktop_window_tree, restore_all_desktop_icons, restore_desktop_icon,
    restore_desktop_icon_async,
};
pub use display::probe_display_topology;
pub use dpi::enable_per_monitor_dpi_v2;
pub use drag_out::{DragOutOutcome, start_file_drag_out};
pub use events::{DisplayEventKind, run_event_monitor};
pub use fence_poc::{FencePocConfig, run_bottom_fence_poc};
pub use folder_watch::{FolderWatcher, WM_APP_PORTAL_DIRTY};
pub use icons::{DecodedIcon, IconCache};
pub use shell_ops::{
    all_desktop_dirs, is_directly_on_desktop, is_vault_path, launch_item,
    migrate_vault_item_to_desktop, pick_folder_dialog, resolve_dropped_item,
    reveal_in_explorer, scan_portal_directory,
};
pub use tray::{TrayIcon, WM_APP_TRAY};
pub use window_attach::{
    DesktopZHookGuard, attach_desktop_owner_and_rounded_corners, find_window_by_exact_title,
    foreground_window_handle, install_desktop_z_win_event_hooks, post_reassert_all_fences_z,
    strip_alt_tab_from_host,
};
