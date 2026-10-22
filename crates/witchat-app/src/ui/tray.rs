//! Notification-area icon, so witchat can keep relaying and receiving with
//! its window closed. Windows, and Linux when a StatusNotifier host (panel)
//! is on the session bus. Elsewhere `Tray::new` returns None and closing
//! the window quits. Linux uses tray-icon's ksni backend, not GTK.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(super) enum TrayAction {
    Open,
    Quit,
}

#[cfg(any(windows, target_os = "linux"))]
pub(super) struct Tray {
    _icon: tray_icon::TrayIcon,
    open: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
    icon_events: std::sync::mpsc::Receiver<tray_icon::TrayIconEvent>,
    menu_events: std::sync::mpsc::Receiver<tray_icon::menu::MenuEvent>,
}

#[cfg(any(windows, target_os = "linux"))]
impl Tray {
    /// Must run on the thread that pumps window messages (the GUI thread).
    pub(super) fn new(png: &[u8]) -> Option<Self> {
        if !host_present() {
            return None;
        }
        match Self::build(png) {
            Ok(tray) => Some(tray),
            Err(err) => {
                tracing::warn!(error = %err, "tray icon unavailable; closing the window quits");
                None
            }
        }
    }

    fn build(png: &[u8]) -> anyhow::Result<Self> {
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
        let image = image::load_from_memory(png)?.resize(32, 32, image::imageops::FilterType::Lanczos3).to_rgba8();
        let icon = tray_icon::Icon::from_rgba(image.to_vec(), image.width(), image.height())?;
        let open = MenuItem::new("open witchat", true, None);
        let quit = MenuItem::new("quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])?;
        let tray = tray_icon::TrayIconBuilder::new()
            .with_tooltip("witchat")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        // A handler replaces tray-icon's own channels; ours also wake the GUI.
        let (icon_tx, icon_events) = std::sync::mpsc::channel();
        tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
            let _ = icon_tx.send(event);
            crate::wake::wake();
        }));
        let (menu_tx, menu_events) = std::sync::mpsc::channel();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |event| {
            let _ = menu_tx.send(event);
            crate::wake::wake();
        }));
        Ok(Self { _icon: tray, open: open.id().clone(), quit: quit.id().clone(), icon_events, menu_events })
    }

    pub(super) fn poll(&self) -> Vec<TrayAction> {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        let mut actions = Vec::new();
        while let Ok(event) = self.icon_events.try_recv() {
            let open = matches!(
                event,
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                    | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
            );
            if open {
                actions.push(TrayAction::Open);
            }
        }
        while let Ok(event) = self.menu_events.try_recv() {
            if event.id == self.open {
                actions.push(TrayAction::Open);
            } else if event.id == self.quit {
                actions.push(TrayAction::Quit);
            }
        }
        actions
    }
}

#[cfg(windows)]
fn host_present() -> bool {
    true
}

/// A tray icon with no panel host would swallow close-to-hide with nothing
/// to click. Only enable the tray when a StatusNotifier watcher is up.
#[cfg(target_os = "linux")]
fn host_present() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/",
        Some("org.freedesktop.DBus"),
        "NameHasOwner",
        &"org.kde.StatusNotifierWatcher",
    )
    .ok()
    .and_then(|reply| reply.body().deserialize().ok())
    .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) struct Tray;

#[cfg(not(any(windows, target_os = "linux")))]
impl Tray {
    pub(super) fn new(_png: &[u8]) -> Option<Self> {
        None
    }

    pub(super) fn poll(&self) -> Vec<TrayAction> {
        Vec::new()
    }
}
