//! Map platform BLE errors into retryable radio-down vs hard failure.

pub fn is_retryable_scan_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("0x800710df")
        || e.contains("0x8007048f")
        || e.contains("0x80070015")
        || e.contains("not ready")
        || e.contains("not available")
        || e.contains("powered off")
        || e.contains("radio not")
        || e.contains("device is not ready")
}

pub fn is_already_scanning(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("already") && (e.contains("scan") || e.contains("watch"))
}

pub fn explain_scan_error(err: &str) -> String {
    if is_retryable_scan_error(err) {
        format!("{} witchat will keep retrying.", radio_off_hint())
    } else {
        format!("BLE scan failed: {err}")
    }
}

pub fn is_closed_link_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("0x80000013")
        || e.contains("has been closed")
        || e.contains("object is closed")
        || e.contains("gattcommunicationstatus(1)")
        || e.contains("unreachable")
}

/// WinRT cancels in-flight GATT operations when the link renegotiates or the
/// phone briefly drops it (`0x800704C7` "operation was canceled by the user").
pub fn is_transient_write_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("0x800704c7") || e.contains("canceled") || e.contains("cancelled")
}

pub fn is_expected_attach_error(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("missing bitchat characteristic") || e.contains("timed out") || e.contains("not connected")
}

pub fn explain_write_error(id: &str, err: &str) -> String {
    if err.to_ascii_lowercase().contains("gattcommunicationstatus(1)") {
        format!("link {id} unreachable (Windows GATT) — reconnecting")
    } else if is_closed_link_error(err) {
        format!("link {id} dropped (GATT session closed) — reconnecting")
    } else {
        format!("write {id}: {err}")
    }
}

pub fn radio_off_hint() -> &'static str {
    #[cfg(windows)]
    {
        "Bluetooth is off or not ready. Turn it on in Settings or the system-tray quick settings."
    }
    #[cfg(not(windows))]
    {
        "Bluetooth is off or not ready. Enable the adapter."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winrt_cancel_is_transient_but_unreachable_is_not() {
        assert!(is_transient_write_error(
            "Error { code: HRESULT(0x800704C7), message: \"The operation was canceled by the user.\" }"
        ));
        assert!(!is_transient_write_error("Error { code: HRESULT(0x80650003) }"));
    }

    #[test]
    fn maps_windows_device_not_ready() {
        let raw = r#"Error { code: HRESULT(0x800710DF), message: "The device is not ready for use." }"#;
        assert!(is_retryable_scan_error(raw));
        let msg = explain_scan_error(raw);
        assert!(msg.contains("Bluetooth"));
        assert!(msg.contains("retry"));
        assert!(!msg.contains("HRESULT"));
    }

    #[test]
    fn leaves_unknown_errors_intact() {
        let raw = "permission denied";
        assert!(!is_retryable_scan_error(raw));
        assert!(explain_scan_error(raw).contains("permission denied"));
    }

    #[test]
    fn treats_already_scanning_as_success() {
        assert!(is_already_scanning("Already scanning"));
        assert!(!is_already_scanning("The device is not ready for use."));
    }

    #[test]
    fn maps_windows_object_closed() {
        let raw = r#"Error { code: HRESULT(0x80000013), message: "The object has been closed." }"#;
        assert!(is_closed_link_error(raw));
        let msg = explain_write_error("aa:bb", raw);
        assert!(msg.contains("reconnect"));
        assert!(!msg.contains("HRESULT"));
    }

    #[test]
    fn maps_gatt_unreachable() {
        let raw = "Windows UWP threw error on write: GattCommunicationStatus(1)";
        assert!(is_closed_link_error(raw));
        assert!(explain_write_error("50:c3", raw).contains("unreachable"));
        assert!(is_expected_attach_error("48:C1 missing bitchat characteristic"));
    }
}
