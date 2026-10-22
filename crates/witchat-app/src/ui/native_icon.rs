//! Title-bar and taskbar icons straight from the exe's `.ico` resource.
//!
//! iced/winit only take one RGBA image and let Windows shrink it, which turns
//! the 256 px logo into mush at 16-24 px. Loading the resource at the
//! window's DPI picks the hand-drawn pixel-art frame instead.

use iced::window::raw_window_handle::{RawWindowHandle, WindowHandle};

/// Resource id of the icon in `witchat.rc`.
#[cfg(windows)]
const ICON_RESOURCE: usize = 1;

pub(super) fn apply(handle: WindowHandle<'_>) {
    let RawWindowHandle::Win32(win) = handle.as_raw() else {
        return;
    };
    #[cfg(windows)]
    if let Err(err) = set_icons(win.hwnd.get()) {
        tracing::warn!(%err, "native window icon not set; using the scaled one");
    }
    #[cfg(not(windows))]
    let _ = win;
}

#[cfg(windows)]
fn set_icons(hwnd: isize) -> windows::core::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON, WM_SETICON,
    };

    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: plain Win32 calls on our own window; the icons stay alive for
    // the process lifetime, as WM_SETICON requires.
    unsafe {
        let module = GetModuleHandleW(None)?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = GetSystemMetricsForDpi(metric, dpi);
            let icon = LoadImageW(
                Some(module.into()),
                PCWSTR(ICON_RESOURCE as *const u16),
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            )?;
            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(which as usize)), Some(LPARAM(icon.0 as isize)));
        }
    }
    Ok(())
}
