//! Keystroke injection abstraction

#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

use dogkbd_proto::KeyTap;

/// Inject a key tap event
#[cfg(windows)]
pub fn inject(tap: &KeyTap) -> Result<(), String> {
    windows::inject(tap)
}

#[cfg(target_os = "linux")]
pub fn inject(tap: &KeyTap) -> Result<(), String> {
    linux::inject(tap)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn inject(_tap: &KeyTap) -> Result<(), String> {
    Err("Injection not supported on this platform".to_string())
}
