//! Embeds the app icon (and version info) into witchat.exe so Explorer, the
//! taskbar and shortcuts show the logo (`assets/witchat.ico`, built from
//! `assets/witchat-logo.png`). Also records the git revision for log headers
//! and diagnostics reports (`unknown` outside a git checkout).

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=witchat.rc");
    println!("cargo:rerun-if-changed=../../assets/witchat.ico");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rustc-env=WITCHAT_GIT_REV={}", git_rev());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("witchat.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("embed witchat.rc (needs the Windows SDK resource compiler)");
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_rev() -> String {
    let Some(rev) = git(&["rev-parse", "--short=12", "HEAD"]).filter(|r| !r.is_empty()) else {
        return "unknown".into();
    };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    if dirty {
        format!("{rev}-dirty")
    } else {
        rev
    }
}
