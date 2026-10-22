//! `cargo run -p witchat-xtask -- dist <binary> <out-dir>`
//!
//! Copies a release binary into `out-dir` with what a stranger needs to
//! trust an unsigned download: `<name>.sha256` (sha256sum format), `<name>.json`
//! (version, git revision, target, toolchain, size, hash) and
//! `THIRD-PARTY-NOTICES.txt` (shipped crates with their licenses).

mod notices;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_PACKAGE: &str = "witchat-app";

#[derive(Serialize)]
struct Manifest {
    name: String,
    version: String,
    git_rev: String,
    git_dirty: bool,
    target: String,
    rustc: String,
    built_utc: String,
    size: u64,
    sha256: String,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, binary, out] if cmd == "dist" => dist(Path::new(binary), Path::new(out)),
        _ => bail!("usage: xtask dist <binary> <out-dir>"),
    }
}

fn dist(binary: &Path, out: &Path) -> Result<()> {
    let name = binary.file_name().and_then(|n| n.to_str()).context("binary has no file name")?.to_string();
    let target = host_target()?;
    let metadata = cargo_metadata(&target)?;
    let blocked = notices::disallowed(&metadata, APP_PACKAGE)?;
    if !blocked.is_empty() {
        bail!("crates outside the license allowlist (xtask/src/notices.rs):\n  {}", blocked.join("\n  "));
    }
    fs::create_dir_all(out)?;
    let dest = out.join(&name);
    fs::copy(binary, &dest).with_context(|| format!("copy {} -> {}", binary.display(), dest.display()))?;

    let bytes = fs::read(&dest)?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    fs::write(sidecar(&dest, "sha256"), format!("{sha256}  {name}\n"))?;

    let manifest = Manifest {
        name: name.clone(),
        version: notices::package_version(&metadata, APP_PACKAGE)?,
        git_rev: git(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "unknown".into()),
        git_dirty: git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(true),
        target,
        rustc: run("rustc", &["--version"])?,
        built_utc: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        size: bytes.len() as u64,
        sha256: sha256.clone(),
    };
    fs::write(sidecar(&dest, "json"), serde_json::to_string_pretty(&manifest)? + "\n")?;
    fs::write(out.join("THIRD-PARTY-NOTICES.txt"), notices::render(&metadata, APP_PACKAGE)?)?;

    if manifest.git_dirty {
        eprintln!("warning: working tree has uncommitted changes; git_rev does not fully describe this build");
    }
    println!("{}  {sha256}  {} bytes", dest.display(), manifest.size);
    Ok(())
}

fn sidecar(path: &Path, ext: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(ext);
    PathBuf::from(name)
}

fn host_target() -> Result<String> {
    run("rustc", &["-vV"])?
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .context("rustc -vV printed no host line")
}

fn cargo_metadata(target: &str) -> Result<serde_json::Value> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let json = run(&cargo, &["metadata", "--format-version", "1", "--locked", "--filter-platform", target])?;
    Ok(serde_json::from_str(&json)?)
}

fn git(args: &[&str]) -> Result<String> {
    run("git", args)
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output().with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!("{program} {} failed: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
