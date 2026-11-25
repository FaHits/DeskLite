use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Enables Per-Monitor DPI Awareness v2 at process startup so monitor rects and
/// `WM_DPICHANGED` callbacks stay crisp across mixed-DPI displays.
pub fn enable_per_monitor_dpi_v2() -> bool {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() }
}
