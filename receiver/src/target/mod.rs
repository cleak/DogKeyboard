//! Window targeting abstraction

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use self::windows::{enumerate_windows, is_foreground, HWND};

/// Information about a window
#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// Platform-specific window handle
    #[cfg(windows)]
    pub hwnd: HWND,
    /// Window title
    pub title: String,
    /// Process name
    pub process_name: String,
}

impl WindowInfo {
    /// Get display name for the window
    pub fn display_name(&self) -> String {
        if self.title.is_empty() {
            self.process_name.clone()
        } else if self.title.chars().count() > 50 {
            let truncated: String = self.title.chars().take(47).collect();
            format!("{}... - {}", truncated, self.process_name)
        } else {
            format!("{} - {}", self.title, self.process_name)
        }
    }
}
