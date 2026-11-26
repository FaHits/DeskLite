use std::collections::HashMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use desklite_core::{DisplayTopology, TopologyError};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::{GUID_CONSOLE_DISPLAY_STATE, GUID_MONITOR_POWER_ON};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW,
    DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, KillTimer, MSG,
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE,
    PostQuitMessage, REGISTER_NOTIFICATION_FLAGS, RegisterClassExW, SPI_SETWORKAREA, SetTimer,
    SetWindowLongPtrW, TranslateMessage, UnregisterClassW, WM_CREATE, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_ENDSESSION, WM_NCDESTROY, WM_POWERBROADCAST, WM_QUERYENDSESSION,
    WM_SETTINGCHANGE, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
    WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::Win32Error;
use crate::display::probe_display_topology;
use crate::win_err;

const DEBOUNCE_TIMER_ID: usize = 1;
const DEBOUNCE_MS: u32 = 150;
const TRANSIENT_RETRY_TIMER_ID: usize = 2;
const TRANSIENT_RETRY_MS: u32 = 350;
const MAX_TRANSIENT_RETRIES: u8 = 8;

#[derive(Default)]
struct PowerSessionReg {
    power_handles: Vec<isize>,
    wts_registered: bool,
}

static POWER_SESSION_REGS: OnceLock<Mutex<HashMap<isize, PowerSessionReg>>> = OnceLock::new();

fn power_session_regs() -> &'static Mutex<HashMap<isize, PowerSessionReg>> {
    POWER_SESSION_REGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Idempotently registers display power-state (`GUID_CONSOLE_DISPLAY_STATE`,
/// `GUID_MONITOR_POWER_ON`) and lock/unlock session (`WTSRegisterSessionNotification`)
/// notifications for `hwnd`.
pub fn register_power_and_session_notifications(hwnd: HWND) {
    if hwnd.is_invalid() {
        return;
    }
    let key = hwnd.0 as isize;
    let mut map = power_session_regs().lock().unwrap_or_else(|e| e.into_inner());
    if map.contains_key(&key) {
        return;
    }
    let mut reg = PowerSessionReg::default();
    let flags = REGISTER_NOTIFICATION_FLAGS(DEVICE_NOTIFY_WINDOW_HANDLE.0);
    for guid in [GUID_CONSOLE_DISPLAY_STATE, GUID_MONITOR_POWER_ON] {
        if let Ok(h) = unsafe { RegisterPowerSettingNotification(HANDLE(hwnd.0), &guid, flags) }
            && h.0 != 0
        {
            reg.power_handles.push(h.0);
        }
    }
    if unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }.is_ok() {
        reg.wts_registered = true;
    }
    map.insert(key, reg);
}

/// Unregisters power and session notifications previously registered for `hwnd`.
pub fn unregister_power_and_session_notifications(hwnd: HWND) {
    if hwnd.is_invalid() {
        return;
    }
    let key = hwnd.0 as isize;
    let reg = power_session_regs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    if let Some(reg) = reg {
        for raw_h in reg.power_handles {
            if raw_h != 0 {
                unsafe {
                    let _ = UnregisterPowerSettingNotification(HPOWERNOTIFY(raw_h));
                }
            }
        }
        if reg.wts_registered {
            unsafe {
                let _ = WTSUnRegisterSessionNotification(hwnd);
            }
        }
    }
}

/// RAII guard that registers power/session notifications for `hwnd` and unregisters on drop.
pub struct PowerSessionNotificationGuard(HWND);

impl PowerSessionNotificationGuard {
    pub fn register(hwnd: HWND) -> Self {
        register_power_and_session_notifications(hwnd);
        Self(hwnd)
    }
}

impl Drop for PowerSessionNotificationGuard {
    fn drop(&mut self) {
        unregister_power_and_session_notifications(self.0);
    }
}

/// Returns `true` when a `WM_POWERBROADCAST` message indicates system/display wake or display power-on.
pub fn is_display_wake_power_broadcast(wparam: WPARAM, lparam: LPARAM) -> bool {
    let code = wparam.0 as u32;
    if code == PBT_APMRESUMEAUTOMATIC || code == PBT_APMRESUMESUSPEND {
        return true;
    }
    if code == PBT_POWERSETTINGCHANGE && lparam.0 != 0 {
        let pbs = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
        if (pbs.PowerSetting == GUID_CONSOLE_DISPLAY_STATE
            || pbs.PowerSetting == GUID_MONITOR_POWER_ON)
            && pbs.DataLength >= size_of::<u32>() as u32
        {
            // POWERBROADCAST_SETTING.Data is a trailing flexible-array member declared as [u8; 1].
            let state = unsafe { std::ptr::read_unaligned(pbs.Data.as_ptr().cast::<u32>()) };
            return state != 0;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub enum DisplayEventKind {
    InitialTopology(DisplayTopology),
    TopologyChanged {
        reason: &'static str,
        topology: DisplayTopology,
    },
    ZeroMonitorTransientIgnored {
        reason: &'static str,
    },
    PowerSuspend,
    SessionEnding,
}

struct MonitorState {
    baseline: Option<DisplayTopology>,
    pending_reason: &'static str,
    last_event_at: Instant,
    transient_retries: u8,
    state_attached: Arc<AtomicBool>,
    on_event: Box<dyn FnMut(DisplayEventKind) -> bool>,
}

fn schedule_reprobe(hwnd: HWND, state: &mut MonitorState, reason: &'static str) {
    state.pending_reason = reason;
    state.last_event_at = Instant::now();
    state.transient_retries = 0;
    let _ = unsafe { KillTimer(Some(hwnd), TRANSIENT_RETRY_TIMER_ID) };
    unsafe { SetTimer(Some(hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
}

fn is_relevant_setting_change(wparam: WPARAM, lparam: LPARAM) -> bool {
    if wparam.0 == SPI_SETWORKAREA.0 as usize {
        return true;
    }
    if lparam.0 != 0 {
        let ptr = lparam.0 as *const u16;
        let mut len = 0usize;
        while len < 64 && unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
        return matches!(
            s.as_str(),
            "TraySettings" | "ShellState" | "WindowMetrics" | "Desktop"
        );
    }
    false
}

fn trigger_reprobe(hwnd: HWND, state: &mut MonitorState, reason: &'static str) {
    let _ = unsafe { KillTimer(Some(hwnd), DEBOUNCE_TIMER_ID) };
    match probe_display_topology() {
        Ok(topo) => {
            state.transient_retries = 0;
            let _ = unsafe { KillTimer(Some(hwnd), TRANSIENT_RETRY_TIMER_ID) };
            let changed = state
                .baseline
                .as_ref()
                .is_none_or(|b| b.key() != topo.key());
            state.baseline = Some(topo.clone());
            // Note: even when key matches (e.g. single-monitor wakeup), emit TopologyChanged so
            // callers always validate & clamp work-area bounds rather than early-returning.
            let label = if changed { reason } else { "WakeOrRefreshVerify" };
            if !(state.on_event)(DisplayEventKind::TopologyChanged {
                reason: label,
                topology: topo,
            }) {
                unsafe { PostQuitMessage(0) };
            }
        }
        Err(Win32Error::Topology(TopologyError::EmptyTransient)) => {
            if !(state.on_event)(DisplayEventKind::ZeroMonitorTransientIgnored { reason }) {
                unsafe { PostQuitMessage(0) };
                return;
            }
            if state.transient_retries < MAX_TRANSIENT_RETRIES {
                state.transient_retries += 1;
                unsafe {
                    SetTimer(
                        Some(hwnd),
                        TRANSIENT_RETRY_TIMER_ID,
                        TRANSIENT_RETRY_MS,
                        None,
                    );
                }
            }
        }
        Err(_) => {}
    }
}

unsafe extern "system" fn monitor_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_CREATE {
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        if !cs.lpCreateParams.is_null() {
            let state = unsafe { &*(cs.lpCreateParams as *const MonitorState) };
            state.state_attached.store(true, Ordering::Release);
        }
        register_power_and_session_notifications(hwnd);
        return LRESULT(0);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut MonitorState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_DISPLAYCHANGE => {
            schedule_reprobe(hwnd, state, "WM_DISPLAYCHANGE");
            LRESULT(0)
        }
        WM_DPICHANGED => {
            schedule_reprobe(hwnd, state, "WM_DPICHANGED");
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            if is_relevant_setting_change(wparam, lparam) {
                schedule_reprobe(hwnd, state, "WM_SETTINGCHANGE");
            }
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            if wparam.0 as u32 == WTS_SESSION_UNLOCK {
                schedule_reprobe(hwnd, state, "WM_WTSSESSION_CHANGE(UNLOCK)");
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            let code = wparam.0 as u32;
            if code == PBT_APMSUSPEND {
                if !(state.on_event)(DisplayEventKind::PowerSuspend) {
                    unsafe { PostQuitMessage(0) };
                }
            } else if is_display_wake_power_broadcast(wparam, lparam) {
                let reason = if code == PBT_POWERSETTINGCHANGE {
                    "WM_POWERBROADCAST(DISPLAY_ON)"
                } else {
                    "WM_POWERBROADCAST(RESUME)"
                };
                schedule_reprobe(hwnd, state, reason);
            }
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == DEBOUNCE_TIMER_ID => {
            if state.last_event_at.elapsed() >= Duration::from_millis(u64::from(DEBOUNCE_MS / 2)) {
                let reason = state.pending_reason;
                trigger_reprobe(hwnd, state, reason);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TRANSIENT_RETRY_TIMER_ID => {
            let _ = unsafe { KillTimer(Some(hwnd), TRANSIENT_RETRY_TIMER_ID) };
            let reason = state.pending_reason;
            trigger_reprobe(hwnd, state, reason);
            LRESULT(0)
        }
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            let _ = (state.on_event)(DisplayEventKind::SessionEnding);
            LRESULT(1)
        }
        WM_DESTROY => {
            unregister_power_and_session_notifications(hwnd);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unregister_power_and_session_notifications(hwnd);
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            drop(unsafe { Box::from_raw(ptr) });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Runs a 0%-idle-CPU event-driven top-level hidden Win32 message loop (`GetMessageW`)
/// listening for display topology, DPI, power, and session shutdown events.
pub fn run_event_monitor<F>(mut on_event: F) -> Result<(), Win32Error>
where
    F: FnMut(DisplayEventKind) -> bool + 'static,
{
    let initial = probe_display_topology().ok();
    if let Some(ref topo) = initial
        && !on_event(DisplayEventKind::InitialTopology(topo.clone()))
    {
        return Ok(());
    }

    let hinstance = unsafe { GetModuleHandleW(None) }.map_err(|e| win_err("GetModuleHandleW", e))?;
    let class_name = w!("DeskLiteDisplayMonitorWnd");
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(monitor_wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    let registered_here = atom != 0;
    if atom == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
        return Err(Win32Error::Status {
            op: "RegisterClassExW",
            code: unsafe { GetLastError() }.0,
        });
    }

    let state_attached = Arc::new(AtomicBool::new(false));
    let boxed_state = Box::new(MonitorState {
        baseline: initial,
        pending_reason: "Initial",
        last_event_at: Instant::now(),
        transient_retries: 0,
        state_attached: Arc::clone(&state_attached),
        on_event: Box::new(on_event),
    });
    let raw_state = Box::into_raw(boxed_state);

    // Top-level hidden tool window (NOT HWND_MESSAGE) so Win32 broadcasts WM_DISPLAYCHANGE /
    // WM_POWERBROADCAST / WM_SETTINGCHANGE reliably.
    let hwnd_res = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name,
            w!("DeskLite Display Monitor"),
            WS_POPUP,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            Some(raw_state.cast()),
        )
    };
    let hwnd = match hwnd_res {
        Ok(h) => h,
        Err(e) => {
            if !state_attached.load(Ordering::Acquire) {
                drop(unsafe { Box::from_raw(raw_state) });
            }
            if registered_here {
                unsafe {
                    let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance.into()));
                }
            }
            return Err(win_err("CreateWindowExW(Monitor)", e));
        }
    };

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        let _ = DestroyWindow(hwnd);
        if registered_here {
            let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance.into()));
        }
    }
    Ok(())
}
