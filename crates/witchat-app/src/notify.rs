//! Notifications for private messages and mentions. They can show on a lock
//! screen and in the notification history, so they name the sender and the
//! chat, never the message text.
//!
//! Windows: WinRT toasts behind AppUserModelID `witchat.desktop`.
//! Linux: `org.freedesktop.Notifications`. Elsewhere `Notifier::new`
//! returns None.

use std::sync::mpsc::{channel, Receiver, Sender};

/// Escape text for the toast XML.
#[cfg(any(windows, test))]
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(any(windows, test))]
fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    )
}

pub struct Notifier {
    clicked_tx: Sender<u32>,
    clicked: Receiver<u32>,
    #[cfg(windows)]
    toaster: windows::UI::Notifications::ToastNotifier,
}

impl Notifier {
    pub fn new() -> Option<Self> {
        let (clicked_tx, clicked) = channel();
        #[cfg(windows)]
        {
            match win::toaster() {
                Ok(toaster) => Some(Self { clicked_tx, clicked, toaster }),
                Err(err) => {
                    tracing::warn!(error = %err, "notifications unavailable");
                    None
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            if !linux::available() {
                return None;
            }
            Some(Self { clicked_tx, clicked })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (clicked_tx, clicked);
            None
        }
    }

    /// Show a toast; a newer one with the same `group` replaces it.
    /// Clicking it reports `tag` through `clicked`.
    pub fn show(&self, tag: u32, group: &str, title: &str, body: &str) {
        #[cfg(windows)]
        if let Err(err) = win::show(&self.toaster, &self.clicked_tx, tag, group, &toast_xml(title, body)) {
            tracing::warn!(error = %err, "toast not shown");
        }
        #[cfg(target_os = "linux")]
        if let Err(err) = linux::show(&self.clicked_tx, tag, group, title, body) {
            tracing::warn!(error = %err, "notification not shown");
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = (tag, group, title, body);
    }

    pub fn clicked(&self) -> Vec<u32> {
        self.clicked.try_iter().collect()
    }

    /// Panic wipe: remove our toasts from the notification history.
    pub fn clear_history(&self) {
        #[cfg(windows)]
        if let Err(err) = win::clear_history() {
            tracing::warn!(error = %err, "could not clear notification history");
        }
        #[cfg(target_os = "linux")]
        linux::clear_history();
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use notify_rust::{Hint, Notification, Timeout};
    use std::sync::mpsc::Sender;
    use std::sync::Mutex;

    static LAST_IDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    pub fn available() -> bool {
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return false;
        };
        conn.call_method(
            Some("org.freedesktop.DBus"),
            "/",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &"org.freedesktop.Notifications",
        )
        .ok()
        .and_then(|reply| reply.body().deserialize().ok())
        .unwrap_or(false)
    }

    pub fn show(clicked: &Sender<u32>, tag: u32, group: &str, title: &str, body: &str) -> anyhow::Result<()> {
        let id = group_id(group);
        let handle = Notification::new()
            .summary(title)
            .body(body)
            .appname("witchat")
            .id(id)
            .action("default", "open")
            .hint(Hint::Resident(false))
            .timeout(Timeout::Milliseconds(8000))
            .show()?;
        if let Ok(mut ids) = LAST_IDS.lock() {
            ids.retain(|existing| *existing != id);
            ids.push(id);
            if ids.len() > 64 {
                ids.remove(0);
            }
        }
        let clicked = clicked.clone();
        std::thread::Builder::new().name("witchat-notify".into()).spawn(move || {
            handle.wait_for_action(move |action| {
                if action == "default" {
                    let _ = clicked.send(tag);
                    crate::wake::wake();
                }
            });
        })?;
        Ok(())
    }

    pub fn clear_history() {
        let ids = LAST_IDS.lock().ok().map(|mut ids| std::mem::take(&mut *ids)).unwrap_or_default();
        let Ok(conn) = zbus::blocking::Connection::session() else {
            return;
        };
        for id in ids {
            let _ = conn.call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "CloseNotification",
                &id,
            );
        }
    }

    fn group_id(group: &str) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        group.hash(&mut hasher);
        // 0 is "let the server assign"; keep a non-zero id so a later
        // notification with the same group replaces the previous one.
        hasher.finish() as u32 | 1
    }
}

#[cfg(windows)]
mod win {
    use std::sync::mpsc::Sender;
    use windows::core::{IInspectable, Ref, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        REG_SZ,
    };
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};

    const AUMID: &str = "witchat.desktop";
    const ICON: &[u8] = include_bytes!("../../../assets/witchat-256.png");

    pub fn toaster() -> anyhow::Result<ToastNotifier> {
        register()?;
        Ok(ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?)
    }

    pub fn show(
        toaster: &ToastNotifier,
        clicked: &Sender<u32>,
        tag: u32,
        group: &str,
        xml: &str,
    ) -> anyhow::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetGroup(&HSTRING::from("witchat"))?;
        toast.SetTag(&HSTRING::from(group))?;
        let clicked = clicked.clone();
        toast.Activated(&TypedEventHandler::new(move |_: Ref<'_, ToastNotification>, _: Ref<'_, IInspectable>| {
            let _ = clicked.send(tag);
            crate::wake::wake();
            Ok(())
        }))?;
        toaster.Show(&toast)?;
        Ok(())
    }

    pub fn clear_history() -> anyhow::Result<()> {
        ToastNotificationManager::History()?.ClearWithId(&HSTRING::from(AUMID))?;
        Ok(())
    }

    /// Name and icon shown on our toasts. Rewritten each start so a moved
    /// install keeps a valid icon path.
    fn register() -> anyhow::Result<()> {
        let icon = crate::app_dirs::data_local_dir()
            .ok_or_else(|| anyhow::anyhow!("no data directory for the toast icon"))?
            .join("witchat-toast.png");
        if !icon.exists() {
            if let Some(dir) = icon.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&icon, ICON)?;
        }
        let path = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
        let mut key = HKEY::default();
        unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                &path,
                None,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut key,
                None,
            )
            .ok()?;
            let result = set_string(key, "DisplayName", "witchat")
                .and_then(|_| set_string(key, "IconUri", &icon.to_string_lossy()));
            let _ = RegCloseKey(key);
            result
        }
    }

    unsafe fn set_string(key: HKEY, name: &str, value: &str) -> anyhow::Result<()> {
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
        RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes)).ok()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_escaped_in_the_toast() {
        let xml = toast_xml("witchat", "private message from <b>&\"x\"");
        assert!(xml.contains("from &lt;b&gt;&amp;&quot;x&quot;"));
        assert!(!xml.contains("<b>"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "shows a real Windows notification"]
    fn live_toast_is_shown() {
        let toaster = win::toaster().expect("toast notifier");
        let (tx, _rx) = channel();
        win::show(&toaster, &tx, 1, "test", &toast_xml("witchat", "test notification from the test suite"))
            .expect("shown");
    }
}
