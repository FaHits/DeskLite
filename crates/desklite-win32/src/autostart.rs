use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::{Win32Error, win_err};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE_NAME: &str = "DeskLite";
const META_KEY_PATH: &str = "Software\\DeskLite";
const META_HIGH_VALUE_NAME: &str = "HighPriorityTarget";
pub const HIGH_PRIORITY_TASK_NAME: &str = "DeskLiteAutostart";

pub const ARG_INSTALL_HIGH_AUTOSTART: &str = "--install-high-autostart";
pub const ARG_UNINSTALL_HIGH_AUTOSTART: &str = "--uninstall-high-autostart";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartMode {
    Disabled,
    Normal,
    HighPriority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartStatus {
    pub mode: AutostartMode,
    pub current_exe: String,
    pub registered_exe: Option<String>,
    pub path_mismatch: bool,
    pub is_elevated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartSetOutcome {
    pub status: AutostartStatus,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElevatedRunResult {
    Success,
    Cancelled,
}

struct RegKeyGuard(HKEY);

impl Drop for RegKeyGuard {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }
}

pub fn is_process_elevated() -> bool {
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut ret_len = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
    }
    .is_ok();
    unsafe {
        let _ = CloseHandle(token);
    }
    ok && elevation.TokenIsElevated != 0
}

pub fn current_exe_display_path() -> String {
    match std::env::current_exe() {
        Ok(p) => strip_verbatim_prefix(&p.to_string_lossy()),
        Err(_) => String::new(),
    }
}

fn strip_verbatim_prefix(raw: &str) -> String {
    raw.strip_prefix("\\\\?\\").unwrap_or(raw).to_string()
}

pub fn normalize_exe_command_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = if trimmed.starts_with('"') {
        trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.split('"').next())
            .unwrap_or(trimmed)
    } else {
        trimmed
    };
    strip_verbatim_prefix(unquoted.trim())
}

fn paths_match_windows(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

fn current_user_principal_id() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    if !domain.is_empty() && !user.is_empty() {
        format!("{domain}\\{user}")
    } else {
        user
    }
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn xml_unescape(input: &str) -> String {
    input
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Builds the Windows Task Scheduler XML for Wallpaper Engine-style fast logon startup:
/// - `LogonTrigger` with zero delay bypasses Explorer's ~10s `HKCU\Run` startup throttle.
/// - `Priority = 4` elevates process scheduling above Task Scheduler's default `7` (`BELOW_NORMAL`).
/// - `RunLevel = LeastPrivilege` runs at interactive Medium integrity so Explorer drag-and-drop
///   (`WM_DROPFILES` / OLE `DoDragDrop`) is not blocked by UIPI.
pub fn build_high_priority_task_xml(user_id: &str, exe_path: &str) -> String {
    let clean_exe = strip_verbatim_prefix(exe_path.trim());
    let work_dir = Path::new(&clean_exe)
        .parent()
        .map(|p| strip_verbatim_prefix(&p.to_string_lossy()))
        .unwrap_or_default();
    let esc_user = xml_escape(user_id.trim());
    let esc_exe = xml_escape(&clean_exe);
    let esc_dir = xml_escape(&work_dir);
    let working_dir_tag = if esc_dir.is_empty() {
        String::new()
    } else {
        format!("\n      <WorkingDirectory>{esc_dir}</WorkingDirectory>")
    };

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\r\n\
  <RegistrationInfo>\r\n\
    <Description>DeskLite High-Priority Logon Autostart</Description>\r\n\
  </RegistrationInfo>\r\n\
  <Triggers>\r\n\
    <LogonTrigger>\r\n\
      <Enabled>true</Enabled>\r\n\
      <UserId>{esc_user}</UserId>\r\n\
    </LogonTrigger>\r\n\
  </Triggers>\r\n\
  <Principals>\r\n\
    <Principal id=\"Author\">\r\n\
      <UserId>{esc_user}</UserId>\r\n\
      <LogonType>InteractiveToken</LogonType>\r\n\
      <RunLevel>LeastPrivilege</RunLevel>\r\n\
    </Principal>\r\n\
  </Principals>\r\n\
  <Settings>\r\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\r\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\r\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\r\n\
    <AllowHardTerminate>true</AllowHardTerminate>\r\n\
    <StartWhenAvailable>true</StartWhenAvailable>\r\n\
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\r\n\
    <IdleSettings>\r\n\
      <StopOnIdleEnd>false</StopOnIdleEnd>\r\n\
      <RestartOnIdle>false</RestartOnIdle>\r\n\
    </IdleSettings>\r\n\
    <AllowStartOnDemand>true</AllowStartOnDemand>\r\n\
    <Enabled>true</Enabled>\r\n\
    <Hidden>false</Hidden>\r\n\
    <RunOnlyIfIdle>false</RunOnlyIfIdle>\r\n\
    <WakeToRun>false</WakeToRun>\r\n\
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\r\n\
    <Priority>4</Priority>\r\n\
  </Settings>\r\n\
  <Actions Context=\"Author\">\r\n\
    <Exec>\r\n\
      <Command>{esc_exe}</Command>{working_dir_tag}\r\n\
    </Exec>\r\n\
  </Actions>\r\n\
</Task>\r\n"
    )
}

pub fn parse_task_xml_command(raw_bytes: &[u8]) -> Option<String> {
    let text = decode_xml_bytes(raw_bytes)?;
    let start_tag = "<Command>";
    let end_tag = "</Command>";
    let start = text.find(start_tag)? + start_tag.len();
    let end = text[start..].find(end_tag)? + start;
    let cmd = xml_unescape(text[start..end].trim());
    if cmd.is_empty() {
        None
    } else {
        Some(normalize_exe_command_path(&cmd))
    }
}

fn decode_xml_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_be_bytes)
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(String::from_utf8_lossy(&bytes[3..]).into_owned());
    }
    if bytes.len() >= 4 && bytes[1] == 0 && bytes[3] == 0 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn read_hkcu_sz(subkey: &str, value_name: &str) -> Option<String> {
    let wsubkey = HSTRING::from(subkey);
    let wname = HSTRING::from(value_name);
    let mut hkey = HKEY::default();
    let st = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &wsubkey, None, KEY_READ, &mut hkey) };
    if st.is_err() {
        return None;
    }
    let _guard = RegKeyGuard(hkey);
    let mut kind = REG_VALUE_TYPE::default();
    let mut bytes = 0u32;
    let st_size = unsafe {
        RegQueryValueExW(
            hkey,
            &wname,
            None,
            Some(&mut kind),
            None,
            Some(&mut bytes),
        )
    };
    if st_size.is_err() || bytes < 2 || kind != REG_SZ {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let mut read_bytes = (buf.len() * 2) as u32;
    let st_read = unsafe {
        RegQueryValueExW(
            hkey,
            &wname,
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr().cast()),
            Some(&mut read_bytes),
        )
    };
    if st_read.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = OsString::from_wide(&buf[..len]).to_string_lossy().into_owned();
    if s.trim().is_empty() { None } else { Some(s) }
}

fn write_hkcu_sz(subkey: &str, value_name: &str, data: &str) -> Result<(), Win32Error> {
    let wsubkey = HSTRING::from(subkey);
    let wname = HSTRING::from(value_name);
    let mut hkey = HKEY::default();
    let st = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &wsubkey,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    if st.is_err() {
        return Err(Win32Error::Status {
            op: "RegCreateKeyExW",
            code: st.0 as u32,
        });
    }
    let _guard = RegKeyGuard(hkey);
    let mut utf16: Vec<u16> = data.encode_utf16().collect();
    utf16.push(0);
    let bytes = unsafe {
        std::slice::from_raw_parts(utf16.as_ptr().cast::<u8>(), utf16.len() * 2)
    };
    let st_set = unsafe { RegSetValueExW(hkey, &wname, None, REG_SZ, Some(bytes)) };
    if st_set.is_err() {
        return Err(Win32Error::Status {
            op: "RegSetValueExW",
            code: st_set.0 as u32,
        });
    }
    Ok(())
}

fn delete_hkcu_value(subkey: &str, value_name: &str) -> Result<(), Win32Error> {
    let wsubkey = HSTRING::from(subkey);
    let wname = HSTRING::from(value_name);
    let mut hkey = HKEY::default();
    let st = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &wsubkey, None, KEY_SET_VALUE, &mut hkey) };
    if st == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if st.is_err() {
        return Err(Win32Error::Status {
            op: "RegOpenKeyExW(delete)",
            code: st.0 as u32,
        });
    }
    let _guard = RegKeyGuard(hkey);
    let st_del = unsafe { RegDeleteValueW(hkey, &wname) };
    if st_del.is_ok() || st_del == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(Win32Error::Status {
            op: "RegDeleteValueW",
            code: st_del.0 as u32,
        })
    }
}

fn task_file_path() -> PathBuf {
    let windir = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .unwrap_or_else(|| OsString::from("C:\\Windows"));
    PathBuf::from(windir)
        .join("System32")
        .join("Tasks")
        .join(HIGH_PRIORITY_TASK_NAME)
}

fn high_priority_task_exists_and_command() -> (bool, Option<String>) {
    if let Ok(output) = Command::new("schtasks")
        .args(["/Query", "/TN", HIGH_PRIORITY_TASK_NAME, "/XML"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        && output.status.success()
    {
        let cmd = parse_task_xml_command(&output.stdout);
        return (true, cmd);
    }

    // Even when a non-elevated user lacks read ACL on `System32\Tasks\DeskLiteAutostart`,
    // `metadata()` returns `PermissionDenied` if the task exists vs `NotFound` if absent.
    let task_exists = match std::fs::metadata(task_file_path()) {
        Ok(_) => true,
        Err(err) => err.kind() == ErrorKind::PermissionDenied,
    };
    if task_exists {
        let meta_path = read_hkcu_sz(META_KEY_PATH, META_HIGH_VALUE_NAME)
            .map(|s| normalize_exe_command_path(&s));
        return (true, meta_path);
    }
    (false, None)
}

pub fn query_autostart_status() -> AutostartStatus {
    let current_exe = current_exe_display_path();
    let is_elevated = is_process_elevated();

    let (high_exists, high_cmd) = high_priority_task_exists_and_command();
    if high_exists {
        let path_mismatch = high_cmd
            .as_deref()
            .is_some_and(|reg| !paths_match_windows(reg, &current_exe));
        return AutostartStatus {
            mode: AutostartMode::HighPriority,
            current_exe,
            registered_exe: high_cmd,
            path_mismatch,
            is_elevated,
        };
    }

    if let Some(raw_run) = read_hkcu_sz(RUN_KEY_PATH, RUN_VALUE_NAME) {
        let reg_exe = normalize_exe_command_path(&raw_run);
        let path_mismatch = !paths_match_windows(&reg_exe, &current_exe);
        return AutostartStatus {
            mode: AutostartMode::Normal,
            current_exe,
            registered_exe: Some(reg_exe),
            path_mismatch,
            is_elevated,
        };
    }

    AutostartStatus {
        mode: AutostartMode::Disabled,
        current_exe,
        registered_exe: None,
        path_mismatch: false,
        is_elevated,
    }
}

fn encode_utf16_le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(2 + text.len() * 2);
    bytes.push(0xFF);
    bytes.push(0xFE);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

pub fn install_high_priority_task_direct(exe_path: &str, user_id: &str) -> Result<(), Win32Error> {
    let effective_user = if user_id.trim().is_empty() {
        current_user_principal_id()
    } else {
        user_id.trim().to_string()
    };
    let xml = build_high_priority_task_xml(&effective_user, exe_path);
    let temp_xml = std::env::temp_dir().join(format!(
        "desklite_autostart_{}.xml",
        std::process::id()
    ));
    std::fs::write(&temp_xml, encode_utf16_le_with_bom(&xml)).map_err(|_| Win32Error::Status {
        op: "write_autostart_xml",
        code: 5,
    })?;
    let output = Command::new("schtasks")
        .arg("/Create")
        .arg("/TN")
        .arg(HIGH_PRIORITY_TASK_NAME)
        .arg("/XML")
        .arg(&temp_xml)
        .arg("/F")
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&temp_xml);

    let output = output.map_err(|_| Win32Error::Status {
        op: "schtasks_create",
        code: 2,
    })?;
    if !output.status.success() {
        return Err(Win32Error::Status {
            op: "schtasks_create",
            code: output.status.code().unwrap_or(1) as u32,
        });
    }
    Ok(())
}

pub fn uninstall_high_priority_task_direct() -> Result<(), Win32Error> {
    let (exists, _) = high_priority_task_exists_and_command();
    if !exists {
        return Ok(());
    }
    let output = Command::new("schtasks")
        .args(["/Delete", "/TN", HIGH_PRIORITY_TASK_NAME, "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| Win32Error::Status {
            op: "schtasks_delete",
            code: 2,
        })?;
    if !output.status.success() {
        let (still_exists, _) = high_priority_task_exists_and_command();
        if still_exists {
            return Err(Win32Error::Status {
                op: "schtasks_delete",
                code: output.status.code().unwrap_or(1) as u32,
            });
        }
    }
    Ok(())
}

fn quote_win_arg(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            backslashes += 1;
        } else if ch == '"' {
            for _ in 0..(backslashes * 2 + 1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            backslashes = 0;
            out.push(ch);
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

fn elevate_self_and_wait(params: &str) -> Result<ElevatedRunResult, Win32Error> {
    let exe = current_exe_display_path();
    let wexe = HSTRING::from(exe.as_str());
    let wparams = HSTRING::from(params);
    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(wexe.as_ptr()),
        lpParameters: PCWSTR(wparams.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(err) = unsafe { ShellExecuteExW(&mut sei) } {
        let hresult_cancelled = windows::core::HRESULT::from_win32(ERROR_CANCELLED.0);
        if err.code() == hresult_cancelled {
            return Ok(ElevatedRunResult::Cancelled);
        }
        return Err(win_err("ShellExecuteExW(runas)", err));
    }
    let hproc = sei.hProcess;
    if hproc.is_invalid() {
        return Err(Win32Error::Status {
            op: "ShellExecuteExW(runas_no_handle)",
            code: 1,
        });
    }
    unsafe {
        let _ = WaitForSingleObject(hproc, INFINITE);
    }
    let mut exit_code = 1u32;
    let got_code = unsafe { GetExitCodeProcess(hproc, &mut exit_code) }.is_ok();
    unsafe {
        let _ = CloseHandle(hproc);
    }
    if !got_code || exit_code != 0 {
        return Err(Win32Error::Status {
            op: "elevated_autostart_helper",
            code: exit_code,
        });
    }
    Ok(ElevatedRunResult::Success)
}

fn remove_high_priority_task_if_needed() -> Result<ElevatedRunResult, Win32Error> {
    let (exists, _) = high_priority_task_exists_and_command();
    if !exists {
        return Ok(ElevatedRunResult::Success);
    }
    if is_process_elevated() {
        uninstall_high_priority_task_direct()?;
        return Ok(ElevatedRunResult::Success);
    }
    elevate_self_and_wait(ARG_UNINSTALL_HIGH_AUTOSTART)
}

pub fn set_autostart_mode(mode: AutostartMode) -> Result<AutostartSetOutcome, Win32Error> {
    let current_exe = current_exe_display_path();
    if current_exe.is_empty() {
        return Err(Win32Error::Status {
            op: "current_exe",
            code: 2,
        });
    }
    match mode {
        AutostartMode::Disabled => {
            if remove_high_priority_task_if_needed()? == ElevatedRunResult::Cancelled {
                return Ok(AutostartSetOutcome {
                    status: query_autostart_status(),
                    cancelled: true,
                });
            }
            let _ = delete_hkcu_value(RUN_KEY_PATH, RUN_VALUE_NAME);
            let _ = delete_hkcu_value(META_KEY_PATH, META_HIGH_VALUE_NAME);
        }
        AutostartMode::Normal => {
            if remove_high_priority_task_if_needed()? == ElevatedRunResult::Cancelled {
                return Ok(AutostartSetOutcome {
                    status: query_autostart_status(),
                    cancelled: true,
                });
            }
            let _ = delete_hkcu_value(META_KEY_PATH, META_HIGH_VALUE_NAME);
            let quoted = format!("\"{current_exe}\"");
            write_hkcu_sz(RUN_KEY_PATH, RUN_VALUE_NAME, &quoted)?;
        }
        AutostartMode::HighPriority => {
            let user_id = current_user_principal_id();
            if is_process_elevated() {
                install_high_priority_task_direct(&current_exe, &user_id)?;
            } else {
                let params = format!(
                    "{} {} {}",
                    ARG_INSTALL_HIGH_AUTOSTART,
                    quote_win_arg(&current_exe),
                    quote_win_arg(&user_id),
                );
                if elevate_self_and_wait(&params)? == ElevatedRunResult::Cancelled {
                    return Ok(AutostartSetOutcome {
                        status: query_autostart_status(),
                        cancelled: true,
                    });
                }
            }
            write_hkcu_sz(META_KEY_PATH, META_HIGH_VALUE_NAME, &current_exe)?;
            let _ = delete_hkcu_value(RUN_KEY_PATH, RUN_VALUE_NAME);
        }
    }
    Ok(AutostartSetOutcome {
        status: query_autostart_status(),
        cancelled: false,
    })
}

/// Handles internal elevated CLI flags before `acquire_single_instance()` is called.
/// Returns `Some(exit_code)` when `desklite-app.exe` was invoked strictly as an autostart helper.
pub fn handle_autostart_cli_args(args: &[String]) -> Option<i32> {
    if args.len() >= 2 && args[1] == ARG_INSTALL_HIGH_AUTOSTART {
        let exe_path = args.get(2).map(|s| s.as_str()).unwrap_or("");
        let user_id = args.get(3).map(|s| s.as_str()).unwrap_or("");
        if exe_path.trim().is_empty() {
            return Some(2);
        }
        return match install_high_priority_task_direct(exe_path, user_id) {
            Ok(()) => Some(0),
            Err(_) => Some(1),
        };
    }
    if args.len() >= 2 && args[1] == ARG_UNINSTALL_HIGH_AUTOSTART {
        return match uninstall_high_priority_task_direct() {
            Ok(()) => Some(0),
            Err(_) => Some(1),
        };
    }
    let _ = PWSTR::null();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_sets_fast_priority_least_privilege_and_escapes_fields() {
        let xml = build_high_priority_task_xml(
            "PC&DOM\\User<1>",
            "C:\\Tools & Apps\\DeskLite\\desklite-app.exe",
        );
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<Priority>4</Priority>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<UserId>PC&amp;DOM\\User&lt;1&gt;</UserId>"));
        assert!(xml.contains("<Command>C:\\Tools &amp; Apps\\DeskLite\\desklite-app.exe</Command>"));
        assert!(xml.contains("<WorkingDirectory>C:\\Tools &amp; Apps\\DeskLite</WorkingDirectory>"));
    }

    #[test]
    fn parse_task_xml_command_handles_utf16_le_and_entities() {
        let xml = build_high_priority_task_xml("DOM\\User", "D:\\Apps & Co\\desklite-app.exe");
        let bytes = encode_utf16_le_with_bom(&xml);
        let parsed = parse_task_xml_command(&bytes);
        assert_eq!(parsed.as_deref(), Some("D:\\Apps & Co\\desklite-app.exe"));
    }

    #[test]
    fn normalize_exe_command_path_strips_quotes_and_verbatim_prefix() {
        assert_eq!(
            normalize_exe_command_path("  \"\\\\?\\E:\\Tools\\desklite-app.exe\" --flag "),
            "E:\\Tools\\desklite-app.exe"
        );
        assert_eq!(
            normalize_exe_command_path("E:\\Tools\\desklite-app.exe"),
            "E:\\Tools\\desklite-app.exe"
        );
    }

    #[test]
    fn handle_autostart_cli_args_ignores_normal_launch() {
        let args = vec!["desklite-app.exe".to_string()];
        assert_eq!(handle_autostart_cli_args(&args), None);
    }
}
