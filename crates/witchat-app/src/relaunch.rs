//! Starting a fresh witchat after a Bluetooth reset, the same as quitting,
//! switching Bluetooth off and on, and starting it again by hand. The new
//! process waits for this one to release the single-instance lock.

use std::time::Duration;

const ENV: &str = "WITCHAT_RELAUNCHED";
/// How long a relaunched witchat waits for the old one to exit.
pub const WAIT: Duration = Duration::from_secs(15);

/// Start witchat again with the same arguments; the caller then exits.
pub fn spawn() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let args = std::env::args_os().skip(1).filter(|a| a != "--background");
    std::process::Command::new(exe).args(args).env(ENV, "1").spawn().map(|_| ())
}

/// This process was started by [`spawn`].
pub fn is_relaunch() -> bool {
    std::env::var_os(ENV).is_some()
}
