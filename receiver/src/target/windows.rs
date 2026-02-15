//! Windows window targeting implementation

use super::WindowInfo;
pub use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::{BOOL, LPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
};
use windows::core::PWSTR;

/// Get list of visible windows
pub fn enumerate_windows() -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();

    unsafe {
        let _ = EnumWindows(
            Some(enum_callback),
            LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
        );
    }

    windows
}

unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: We pass a valid pointer to Vec<WindowInfo> in enumerate_windows
    unsafe {
        let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);

        // Skip invisible windows
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        // Get window title
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len == 0 {
            return BOOL(1);
        }

        let mut title_buf = vec![0u16; (title_len + 1) as usize];
        let actual_len = GetWindowTextW(hwnd, &mut title_buf);
        if actual_len == 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&title_buf[..actual_len as usize]);

        // Get process name
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let process_name = get_process_name(pid).unwrap_or_else(|| "Unknown".to_string());

        windows.push(WindowInfo {
            hwnd,
            title,
            process_name,
        });

        BOOL(1)
    }
}

fn get_process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 260];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(handle);
        if result.is_ok() {
            let path = String::from_utf16_lossy(&buf[..size as usize]);
            // Extract just the filename
            path.rsplit('\\').next().map(|s| s.to_string())
        } else {
            None
        }
    }
}

/// Check if a specific window is currently the foreground window
pub fn is_foreground(hwnd: HWND) -> bool {
    unsafe { GetForegroundWindow() == hwnd }
}

/// Bring a window to the foreground
pub fn set_foreground(hwnd: HWND) {
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
}
