use std::collections::HashMap;
use std::mem::size_of;

use desklite_core::{DisplayTopology, MonitorDescriptor, Rect};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QDC_VIRTUAL_MODE_AWARE, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

use crate::Win32Error;

#[derive(Debug, Clone)]
struct CcdTargetInfo {
    device_path: String,
    friendly_name: String,
}

fn utf16_z_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn query_ccd_device_map() -> HashMap<String, CcdTargetInfo> {
    let mut map = HashMap::new();
    let flags = QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE;
    let mut num_paths = 0u32;
    let mut num_modes = 0u32;

    let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut num_paths, &mut num_modes) };
    if rc != ERROR_SUCCESS || num_paths == 0 {
        return map;
    }

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
    let rc = unsafe {
        QueryDisplayConfig(
            flags,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return map;
    }
    paths.truncate(num_paths as usize);

    for path in &paths {
        let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        let src_ok = unsafe { DisplayConfigGetDeviceInfo(&mut src.header) } == ERROR_SUCCESS.0 as i32;
        if !src_ok {
            continue;
        }
        let gdi_name = utf16_z_to_string(&src.viewGdiDeviceName);
        if gdi_name.is_empty() {
            continue;
        }

        let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        let tgt_ok = unsafe { DisplayConfigGetDeviceInfo(&mut tgt.header) } == ERROR_SUCCESS.0 as i32;
        let (device_path, friendly_name) = if tgt_ok {
            let dp = utf16_z_to_string(&tgt.monitorDevicePath);
            let fnm = utf16_z_to_string(&tgt.monitorFriendlyDeviceName);
            (
                if dp.is_empty() {
                    format!("{gdi_name}:target-{}", path.targetInfo.id)
                } else {
                    dp
                },
                if fnm.is_empty() {
                    gdi_name.clone()
                } else {
                    fnm
                },
            )
        } else {
            (
                format!("{gdi_name}:target-{}", path.targetInfo.id),
                gdi_name.clone(),
            )
        };

        map.insert(
            gdi_name,
            CcdTargetInfo {
                device_path,
                friendly_name,
            },
        );
    }

    map
}

struct EnumState {
    ccd: HashMap<String, CcdTargetInfo>,
    monitors: Vec<MonitorDescriptor>,
}

unsafe extern "system" fn enum_monitor_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let state = unsafe { &mut *(lparam.0 as *mut EnumState) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
        return TRUE;
    }

    let gdi_name = utf16_z_to_string(&info.szDevice);
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_err() {
        dpi_x = 96;
        dpi_y = 96;
    }

    let ccd_info = state.ccd.get(&gdi_name);
    let device_key = ccd_info
        .map(|c| c.device_path.clone())
        .unwrap_or_else(|| gdi_name.clone());
    let friendly_name = ccd_info
        .map(|c| c.friendly_name.clone())
        .unwrap_or(gdi_name);

    let mr = info.monitorInfo.rcMonitor;
    let wr = info.monitorInfo.rcWork;
    let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

    state.monitors.push(MonitorDescriptor {
        device_key,
        friendly_name,
        rect: Rect::new(mr.left, mr.top, mr.right, mr.bottom),
        work_area: Rect::new(wr.left, wr.top, wr.right, wr.bottom),
        dpi_x,
        dpi_y,
        is_primary,
    });

    TRUE
}

/// Probes the live Windows 11 monitor topology via CCD `QueryDisplayConfig` +
/// `EnumDisplayMonitors` + `GetDpiForMonitor`.
pub fn probe_display_topology() -> Result<DisplayTopology, Win32Error> {
    let mut state = EnumState {
        ccd: query_ccd_device_map(),
        monitors: Vec::new(),
    };
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_cb),
            LPARAM(&mut state as *mut EnumState as isize),
        )
    };
    if !ok.as_bool() && state.monitors.is_empty() {
        return Err(Win32Error::Topology(desklite_core::TopologyError::EmptyTransient));
    }
    Ok(DisplayTopology::try_from_monitors(state.monitors)?)
}
