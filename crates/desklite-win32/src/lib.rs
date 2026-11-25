pub mod desktop_listview;
pub mod display;
pub mod dpi;
pub mod events;
pub mod fence_poc;
pub mod folder_watch;
pub mod icons;
pub mod shell_ops;
pub mod tray;

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
    DesktopIconInfo, DesktopListView, find_desktop_listview, probe_desktop_window_tree,
};
pub use display::probe_display_topology;
pub use dpi::enable_per_monitor_dpi_v2;
pub use events::{DisplayEventKind, run_event_monitor};
pub use fence_poc::{FencePocConfig, run_bottom_fence_poc};
pub use folder_watch::{FolderWatcher, WM_APP_PORTAL_DIRTY};
pub use icons::{DecodedIcon, IconCache};
pub use shell_ops::{
    launch_item, pick_folder_dialog, resolve_dropped_item, reveal_in_explorer,
    scan_portal_directory,
};
pub use tray::{TrayIcon, WM_APP_TRAY};
