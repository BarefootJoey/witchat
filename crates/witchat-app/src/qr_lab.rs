//! `--qr-lab`: forged QR codes for swapped-key experiments against official
//! phones. Each forgery is internally consistent (it passes the
//! QR signature check) but names a throwaway signing key, so the real
//! owner's answer to the challenge cannot match it.
//!
//! - `--qr-lab mine`: our Noise key with a throwaway signer, saved as a
//!   picture for the phone to scan. The phone must NOT mark us verified.
//! - `--qr-lab <url or picture>`: a phone's QR re-signed the same way,
//!   plus a tampered copy that was not re-signed. Paste both into witchat.

use crate::identity_store;
use crate::qr_image::{decode_qr_file, save_qr_png};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use witchat_core::{check_qr, resign_with_fresh_key, MeshRouter};

pub fn run(source: &str) -> Result<()> {
    if source == "mine" {
        forge_mine()
    } else {
        forge_theirs(source)
    }
}

fn forge_mine() -> Result<()> {
    let router = MeshRouter::new(identity_store::load_or_create(None)?);
    let now = now_secs();
    let genuine = check_qr(&router.my_verification_qr(None, now), now).map_err(|e| anyhow::anyhow!("{e}"))?;
    let forged = resign_with_fresh_key(&genuine, now);
    let path = lab_picture("witchat-qr-lab-mine.png");
    save_qr_png(&forged.to_url(), &path).map_err(anyhow::Error::msg)?;
    println!("experiment A: our Noise key, a throwaway signing key");
    println!("  picture: {}", path.display());
    println!("  1. keep the witchat window running and in range of the phone");
    println!("  2. scan this picture with the phone's bitchat (valid 5 minutes)");
    println!("  3. expect: the phone does NOT show witchat as verified;");
    println!("     witchat logs 'verify: answered QR challenge' (our real key answered)");
    open(&path);
    Ok(())
}

fn forge_theirs(source: &str) -> Result<()> {
    let url = if source.starts_with("bitchat://") {
        source.to_string()
    } else {
        decode_qr_file(Path::new(source)).map_err(anyhow::Error::msg)?
    };
    let now = now_secs();
    let genuine =
        check_qr(&url, now).map_err(|e| anyhow::anyhow!("{e}: capture a fresh code (they last 5 minutes)"))?;
    let forged = resign_with_fresh_key(&genuine, now);
    let tampered = genuine.to_url().replacen(
        &format!("nick={}", percent_nick(&genuine.nickname)),
        &format!("nick={}x", percent_nick(&genuine.nickname)),
        1,
    );
    if tampered == genuine.to_url() {
        bail!("could not build the tampered control");
    }
    let path = lab_picture("witchat-qr-lab-theirs.png");
    save_qr_png(&forged.to_url(), &path).map_err(anyhow::Error::msg)?;
    println!("experiment B: {}'s Noise key, a throwaway signing key", genuine.nickname);
    println!("  paste into witchat > settings > VERIFY > validate (or open {}):", path.display());
    println!("{}", forged.to_url());
    println!("  expect: 'could not prove it owns that QR: NOT verified'; no ✓ on the peer");
    println!();
    println!("control: nickname changed without re-signing:");
    println!("{tampered}");
    println!("  expect: 'invalid or expired QR payload'; nothing is sent");
    Ok(())
}

/// Matches how `VerificationQr::to_url` writes the nickname.
fn percent_nick(nick: &str) -> String {
    let url = witchat_protocol::verify::VerificationQr {
        v: 1,
        noise_key_hex: String::new(),
        sign_key_hex: String::new(),
        npub: None,
        nickname: nick.to_string(),
        ts: 0,
        nonce_b64: String::new(),
        sig_hex: String::new(),
    }
    .to_url();
    url.split('&').find_map(|p| p.strip_prefix("nick=")).unwrap_or(nick).to_string()
}

fn lab_picture(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn open(path: &Path) {
    if let Err(err) = std::process::Command::new("explorer").arg(path).spawn() {
        eprintln!("could not open the picture ({err}); open it yourself");
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nick_encoding_matches_the_url_writer() {
        assert_eq!(percent_nick("bob"), "bob");
        assert_eq!(percent_nick("bob x"), "bob%20x");
    }
}
