# Contributing

Witchat 🧙‍♀️ is a desktop peer on the bitchat mesh. Working with the official phones comes first, new features second. For anything bigger than a fix, open an issue before writing code.

Security problems: not in the issue tracker. See [SECURITY.md](SECURITY.md).

## Build and check

Rust 1.90+. Windows also needs the [MSVC Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (Desktop development with C++). Linux needs BlueZ and `sudo apt-get install -y libdbus-1-dev pkg-config libssl-dev`.

```
cargo run -p witchat-app                                  # the app
cargo test --workspace                                    # all tests
cargo fmt --all                                           # formatting
cargo clippy --workspace --all-targets -- -D warnings     # lints, zero warnings
```

CI runs the same on Windows and Linux; Bluetooth code differs per OS, so a PR is ready when both are green. If PowerShell cannot find `cargo`, run `. .\scripts\use-rust.ps1`.

## Layout

| Crate | Owns |
|---|---|
| `witchat-protocol` | Wire format: packets, TLVs, fragments, capabilities. No I/O |
| `witchat-crypto` | Identity, Noise XX, signatures |
| `witchat-core` | Mesh router, peers, private chat, files, relay policy, ping/trace. No radio, no clock thread |
| `witchat-ble` | Bluetooth LE central, and the GATT server on Windows (WinRT) and Linux (BlueZ) |
| `witchat-nostr` | Gift wraps, location channels, relay directory, Tor transport |
| `witchat-app` | Worker thread, iced GUI, settings, outbox, notifications, tray, headless relay |
| `xtask` | `dist`: checksums, provenance, third-party notices, license gate |

## Rules

- **The official iOS/macOS source (Unlicense) is the wire reference.** When you match a behavior, name the iOS type or constant in a comment or test, so it can be re-checked when upstream changes.
- **No code from `bitchat-android`.** It is GPL-3.0.
- **Permissive dependencies only** (MIT, BSD, Apache-2.0, ISC, Zlib and the like). `cargo run -p witchat-xtask -- dist <binary> <dir>` refuses to package anything else; run it after adding a crate.
- **Privacy fails closed.** No keys, tokens or private message text in logs. With Tor on, nothing connects directly. Notifications never show message text. Unsent private messages touch disk only sealed.
- **Windows and Linux ship together** for everything outside the radio and OS integrations.
- **Tests come with the change.** Protocol and routing are tested with two or three in-memory routers; `crates/witchat-core/src/private_chat/tests.rs` shows the pattern.
- **Claims need proof.** If no phone has shown it, the docs do not say it.
- Files stay under ~500 lines and functions short. Comments explain constraints the code cannot show.

## Testing with phones

The mesh can only be proven against official bitchat. Install it from the App Store or Play Store (never from this repo). Write down each phone's model, OS version and bitchat version, and the `build:` line of witchat's diagnostics report.

Run each case from a fresh start of witchat, with the phone's screen on and bitchat open unless the case says otherwise. "Listed" means the sidebar under PEOPLE.

| Case | Steps | Pass when |
|---|---|---|
| P1 Discovery | Phone within 2 m, bitchat open. Start witchat | Phone listed within 30 s; witchat appears on the phone |
| P2 Public chat | Send a `#mesh` line from each side | Each arrives on the other side within 5 s |
| P3 Private chat | Click the phone's name, send a message each way | Both arrive; witchat shows delivered, then read |
| P4 Outbox | Turn the phone's Bluetooth off. Send it a private message from witchat. Turn Bluetooth back on within 10 min | The message arrives without resending by hand |
| P5 Files | Send a photo each way in the private chat | Opens on both sides at full size |
| P6 Walk away | Carry the phone out of range for 2 min, then back | Listed again within 30 s, no restart |
| P7 Background | After P1, put bitchat in the background on the phone for 5 min, then send from witchat | The message arrives; the phone stays listed |
| P8 Hop | Two phones that cannot hear each other (different rooms), the PC in range of both. Send `#mesh` from phone A | Phone B receives it; the diagnostics report shows relayed packets |

## Linux lab

The Linux GATT server and the relay service have only run in CI. With a Linux desktop and one or two phones:

| Case | Steps | Pass when |
|---|---|---|
| L1 Advertising | `./witchat` | Status line reads `advertising bitchat service via BlueZ…`, not `central-only: …` |
| L2 Phone connects in | P1 with the phone | Log shows `phone subscribed to our GATT server`; P2 passes |
| L3 Two phones in | Both phones connected in at once (they share one `in:bluez` link) | Each sees `#mesh` from the PC and from the other |
| L4 Private over the inbound link | P3 and P5 | Both pass |
| L5 Relay service | Set up `witchat --relay` as in [docs/LINUX.md](docs/LINUX.md#headless-relay) | `systemctl --user status witchat-relay` is active; `systemd-inhibit --list` shows witchat on mains power; `journalctl --user -u witchat-relay` prints counts every 15 min |
| L6 One copy | Start the window app while the relay runs | It refuses and names the relay |

## Reporting results

Open an **Interop report** issue for each failing case: the case id, what happened instead, roughly when (UTC), phone models and versions, and the diagnostics report (**settings → debug → copy diagnostics report**, or `witchat --diag`). The report leaves out messages, keys and names. For L cases add your distro and `bluetoothctl --version`. Passing runs are worth a comment too: they tell us which phones work.

## Pull requests

- One topic per commit; the message says what changed for the user.
- The description says what you tested: which cases above, on which OS.
- UI changes include before and after screenshots.
- Contributions are released under the [MIT license](LICENSE).
