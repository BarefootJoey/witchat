//! Ctrl-C or closing the console ends `--scan` and `--relay` without running
//! any drop code, so Bluetooth would stay held by the dead process for
//! minutes (see `BleCentral::shutdown`). Release it first.

#[cfg(windows)]
pub fn install() {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    unsafe extern "system" fn on_ctrl(_event: u32) -> windows::core::BOOL {
        crate::mesh_worker::stop_running_worker();
        // FALSE: the default handler still ends the process.
        windows::core::BOOL(0)
    }

    // SAFETY: `on_ctrl` is a plain function that lives as long as the process.
    if let Err(err) = unsafe { SetConsoleCtrlHandler(Some(on_ctrl), true) } {
        tracing::warn!(%err, "console stop handler unavailable; Ctrl-C will not release Bluetooth first");
    }
}

/// BlueZ drops a process's GATT service as soon as its D-Bus connection ends.
#[cfg(not(windows))]
pub fn install() {}
