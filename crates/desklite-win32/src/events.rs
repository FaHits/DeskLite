use std::time::{Duration, Instant};

use desklite_core::{DisplayTopology, TopologyError};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowLongPtrW, TranslateMessage, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION,
    WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::w;

use crate::Win32Error;
use crate::display::probe_display_topology;
use crate::win_err;

const DEBOUNCE_TIMER_ID: usize = 1;
const DEBOUNCE_MS: u32 = 150;

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
    on_event: Box<dyn FnMut(DisplayEventKind) -> bool>,
}

fn trigger_reprobe(hwnd: HWND, state: &mut MonitorState, reason: &'static str) {
    let _ = unsafe { KillTimer(Some(hwnd), DEBOUNCE_TIMER_ID) };
    match probe_display_topology() {
        Ok(topo) => {
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
        let cs = unsafe { &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA, cs.lpCreateParams as isize) };
        return LRESULT(0);
    }

    let ptr = unsafe { GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA) } as *mut MonitorState;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let state = unsafe { &mut *ptr };

    match msg {
        WM_DISPLAYCHANGE => {
            state.pending_reason = "WM_DISPLAYCHANGE";
            state.last_event_at = Instant::now();
            unsafe { SetTimer(Some(hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_DPICHANGED => {
            state.pending_reason = "WM_DPICHANGED";
            state.last_event_at = Instant::now();
            unsafe { SetTimer(Some(hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            state.pending_reason = "WM_SETTINGCHANGE";
            state.last_event_at = Instant::now();
            unsafe { SetTimer(Some(hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            let code = wparam.0 as u32;
            if code == PBT_APMSUSPEND {
                if !(state.on_event)(DisplayEventKind::PowerSuspend) {
                    unsafe { PostQuitMessage(0) };
                }
            } else if code == PBT_APMRESUMEAUTOMATIC {
                state.pending_reason = "WM_POWERBROADCAST(RESUME)";
                state.last_event_at = Instant::now();
                unsafe { SetTimer(Some(hwnd), DEBOUNCE_TIMER_ID, DEBOUNCE_MS, None) };
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
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            let _ = (state.on_event)(DisplayEventKind::SessionEnding);
            LRESULT(1)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
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
    if atom == 0 {
        return Err(Win32Error::Status {
            op: "RegisterClassExW",
            code: 0,
        });
    }

    let mut boxed_state = Box::new(MonitorState {
        baseline: initial,
        pending_reason: "Initial",
        last_event_at: Instant::now(),
        on_event: Box::new(on_event),
    });

    // Top-level hidden tool window (NOT HWND_MESSAGE) so Win32 broadcasts WM_DISPLAYCHANGE /
    // WM_POWERBROADCAST / WM_SETTINGCHANGE reliably.
    let hwnd = unsafe {
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
            Some((&raw mut *boxed_state).cast()),
        )
    }
    .map_err(|e| win_err("CreateWindowExW(Monitor)", e))?;

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
