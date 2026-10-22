//! Keeps the PC from going to sleep on its own while it relays for the mesh
//! on mains power, so phones nearby keep a relay. Only idle sleep is held
//! off: closing the lid or choosing sleep still works. Windows uses
//! `SetThreadExecutionState` (shown by `powercfg /requests`); Linux a logind
//! `idle` inhibitor (shown by `systemd-inhibit --list`).

use std::sync::mpsc;

#[derive(Default)]
pub struct StayAwake {
    held: Option<Hold>,
}

impl StayAwake {
    /// Take or release the power request to match `want`.
    pub fn set(&mut self, want: bool) {
        if want == self.held.is_some() {
            return;
        }
        self.held = want.then(Hold::take);
        tracing::info!(on = want, "power: stay awake while relaying");
    }

    pub fn active(&self) -> bool {
        self.held.is_some()
    }
}

/// The request lives on its own thread: Windows ties it to the calling
/// thread, and logind's D-Bus call blocks. Dropping this releases it.
struct Hold {
    _release: mpsc::Sender<()>,
}

impl Hold {
    fn take() -> Self {
        let (release, released) = mpsc::channel::<()>();
        let spawned = std::thread::Builder::new().name("stay-awake".into()).spawn(move || match imp::acquire() {
            Ok(guard) => {
                let _ = released.recv();
                drop(guard);
            }
            Err(err) => tracing::warn!(%err, "power: could not keep the PC awake"),
        });
        if let Err(err) = spawned {
            tracing::warn!(%err, "power: could not start the stay-awake thread");
        }
        Self { _release: release }
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};

    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: only changes this thread's execution state.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }

    pub fn acquire() -> Result<Guard, String> {
        // SAFETY: only changes this thread's execution state.
        let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
        if previous.0 == 0 {
            return Err("SetThreadExecutionState failed".into());
        }
        Ok(Guard)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use zbus::blocking::Connection;
    use zbus::zvariant::OwnedFd;

    /// logind releases the inhibitor when this descriptor closes.
    pub struct Guard {
        _fd: OwnedFd,
    }

    pub fn acquire() -> Result<Guard, String> {
        let fail = |e: zbus::Error| format!("logind inhibit: {e}");
        let bus = Connection::system().map_err(fail)?;
        let reply = bus
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("idle", "witchat", "relaying for the bitchat mesh", "block"),
            )
            .map_err(fail)?;
        let fd: OwnedFd = reply.body().deserialize().map_err(fail)?;
        Ok(Guard { _fd: fd })
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    pub struct Guard;

    pub fn acquire() -> Result<Guard, String> {
        Err("not supported on this system".into())
    }
}
