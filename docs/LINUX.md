# Linux

Same Rust workspace. BlueZ is required for BLE.

```
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libdbus-1-dev libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
bash scripts/build-linux.sh
./dist/linux/witchat --scan
```

Add your user to the `bluetooth` group, then log out and back in:

```
sudo usermod -aG bluetooth "$USER"
```

witchat scans for and connects to phones, and also publishes the bitchat GATT service through BlueZ (`GattManager1` + `LEAdvertisingManager1`) so phones can connect to the PC. The status line reads `advertising bitchat service via BlueZ…` or `central-only: <reason>` (adapter off, no advertising support, or a D-Bus policy refusal). A phone that connects in logs `phone writing to our GATT server` and `phone subscribed to our GATT server`; BlueZ notifies all such phones together, so they share one `in:bluez` link. `--no-advertise` turns it off. This is built and compiles in CI but has not been run against a phone on a real Linux desktop yet. The independent [BitChat Desktop](https://github.com/youhanasheriff/bitchat-desktop) Linux preview is GTK4 discovery only; it does not message. Witchat on Linux is the iced messenger from this repo, not that scanner.

The public download is this portable binary in a `.tar.gz`, with `witchat.sha256` beside it, shipped in the same release as the Windows zip. Unzip and run it; there is no distro package. Copying the folder to a USB stick does not replace the checksum.

## Headless relay

`witchat --relay` relays for the mesh with no window, for a box left on. It is its own node: identity, settings and logs live in a `relay` folder under the data dir (`~/.local/share/witchat/relay`), so private messages to your own identity never land on a process nobody reads. While on mains power it holds a logind `idle` inhibitor (`systemd-inhibit --list` shows it); `--allow-sleep` turns that off. Every 15 minutes it logs what it did for the mesh, as counts.

Run it as a user service from the repo (one line each):

1. `mkdir -p ~/.local/bin ~/.config/systemd/user && cp dist/linux/witchat ~/.local/bin/ && cp scripts/witchat-relay.service ~/.config/systemd/user/`
2. `systemctl --user daemon-reload && systemctl --user enable --now witchat-relay && systemctl --user status witchat-relay --no-pager`
3. `sudo loginctl enable-linger "$USER" && loginctl show-user "$USER" -p Linger` (starts at boot without a login)
4. `journalctl --user -u witchat-relay -n 50 --no-pager` (the same lines also go to `~/.local/share/witchat/relay/logs`)

Stop it with `systemctl --user disable --now witchat-relay`. The relay connects to Nostr relays directly unless its own `relay/social.json` has a `tor_proxy` (copy the field from your main `social.json`). Run the relay or the GUI on one machine, not both: they share the Bluetooth adapter, so whichever starts second exits and names the one already running (the unit retries every 10 s, so the relay resumes once the window app quits).

The unit and the logind inhibitor are tested in CI but have not run on a real Linux desktop with a phone yet, any more than the GATT server above. If you have both, the [Linux lab](../CONTRIBUTING.md#linux-lab) steps say what to check.

## Tor (opt-in)

Same witchat setting as Windows ([WINDOWS.md](WINDOWS.md)): settings > **TOR**, then a local SOCKS5 address. witchat does not bundle Tor.

- **Tor Browser:** download from [https://www.torproject.org/download/](https://www.torproject.org/download/), leave it connected, use `127.0.0.1:9150`.
- **System tor:** `sudo apt-get install tor` (or your distro's package). The daemon usually listens on `127.0.0.1:9050`. Confirm with `systemctl status tor` and a log line `Bootstrapped 100%`.

If the proxy stops answering, relay traffic pauses instead of going direct. Green vs orange in settings, and what traffic this covers: [WINDOWS.md](WINDOWS.md#tor-opt-in).

### Disconnect

Turn **route relays through tor** off in settings to send location channels and Nostr DMs direct again (relays see this PC's IP; presence stops). Quitting Tor without that toggle **pauses** relay traffic instead.

- **Tor Browser:** quit the browser (closes port 9150).
- **System tor:** `sudo systemctl stop tor` (until reboot), or `sudo systemctl disable --now tor` so it stays off.

Closing the window leaves witchat in the tray when a notification-area host is running (StatusNotifier / KDE / GNOME panel). Notifications for private messages and mentions use `org.freedesktop.Notifications` and never include the message text.

Identity is `identity.json` under the platform data dir. When a session Secret Service (GNOME Keyring, KWallet, KeePassXC, …) is running, the file is an envelope around ciphertext and the wrap key lives in the keyring; the 24 h DM outbox is sealed the same way. Without a keyring the identity is mode `0600` plaintext and the outbox stays in memory. Windows uses DPAPI for both.

CI builds the Linux binary in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml). Downloads are GitHub Releases built by CI from the tagged commit.
