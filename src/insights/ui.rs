//! Insights window. OWNER: UI worker.

pub fn register(_hinst: windows_sys::Win32::Foundation::HINSTANCE) {}

pub fn open(_a: &mut crate::app::App) {}

/// New data: repaint if the window is open (cheap no-op when closed).
pub fn refresh() {}

pub fn set_dark(_dark: bool) {}
