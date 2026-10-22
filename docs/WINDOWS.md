# Windows

## Bluetooth radio

HRESULT `0x800710DF` (`The device is not ready for use`) means the adapter exists but the radio is off or still coming up. It is not a protocol bug.

1. Open the quick-settings flyout (Win+A) and turn **Bluetooth** on until the tile is highlighted. `Not connected` with a grey tile usually means off.
2. Leave witchat running. It retries the scan on its own; you should not need to restart the exe.
3. If it stays down: toggle Bluetooth off/on, then confirm `bthserv` is running (`Get-Service bthserv`).
4. Close other BLE apps that may hold the watcher.

Only one copy of witchat can use Bluetooth: two would fight over the same phones. Starting the window app again brings the running one to the front; `--relay` or `--scan` next to another copy exits with a message naming it. If several phones are refused in a row, witchat says **Bluetooth on this PC is refusing every bitchat phone**: close any other Bluetooth tool, then toggle Bluetooth off and on. In the sidebar, two minutes with nobody found adds the same tips, plus the one most first runs need: an iPhone must have bitchat open (not in the background) to be found.

witchat always scans for and connects to phones that are advertising. If `BluetoothAdapter.IsPeripheralRoleSupported` is true, it also publishes the bitchat GATT service with WinRT `GattServiceProvider` and advertises it, so phones can connect to the PC. The footer reads `advertising bitchat service…` or `central-only: <reason>`. If it says central-only right after witchat starts, quit and open it again; Windows sometimes refuses the first try. A phone that subscribes appears in the log as `phone subscribed to our GATT server` and becomes a link like any other. One Android phone has connected in this way and chatted (16 Oct 2026). Official phones also mesh over **Wi-Fi Aware**; witchat does not. A phone that only learns about witchat over Wi-Fi will not appear in the peer list until it advertises Bluetooth.

The independent [BitChat Desktop](https://github.com/youhanasheriff/bitchat-desktop) Windows preview is a 30-second advertisement scanner. It does not connect, handshake, or chat. Witchat is the messenger; do not treat that preview as a second PC on the mesh.

## cargo not recognized

A PowerShell session opened before rustup was installed, or without `%USERPROFILE%\.cargo\bin` on PATH, throws `CommandNotFoundException`. The portable `dist\windows\witchat.exe` does not need cargo.

In this repo:

```
. .\scripts\use-rust.ps1
cargo run -p witchat-app
```

Or call cargo by full path:

```
& "$env:USERPROFILE\.cargo\bin\cargo.exe" run -p witchat-app
```

If `cargo.exe` is missing, install [rustup](https://rustup.rs) plus [MSVC Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (Desktop development with C++), then open a **new** terminal.

## Unsigned download: SmartScreen and Defender

`witchat.exe` is not code-signed yet. A Release download warns in the order in [README.md](../README.md#first-run-the-warnings): the browser may call the zip uncommon, SmartScreen says the publisher is unknown, and Defender may block a folder on the Desktop or in OneDrive. Check the hash before you allow it. `Get-FileHash witchat.exe` must print the hash in `witchat.exe.sha256` next to the exe. `SHA256SUMS.txt` on the Release page is the hash of the zip, not of the exe.

- **SmartScreen** ("Windows protected your PC") on first start: click **More info**, check that the file is `witchat.exe`, then **Run anyway**. Windows remembers the choice for that file.
- **Defender** "Access is denied" or "Risky action blocked": see below.

A publisher signature, once a download has one, puts a name on `witchat.exe`. Check the hash anyway. The signature does not remove the ransomware-folder block below, and a new publisher still gets SmartScreen warnings for the first weeks. Copying the folder to a USB stick often skips SmartScreen, because that check cares about files marked as downloaded from the internet, and a typical stick does not keep the mark. The stick is still not a verified build unless the exe is signed and you trust that publisher. Allowing the file is a choice on this PC, not proof the build matches the source. The Linux download is the same kind of portable folder, with `sha256sum -c`.

## Defender: "Risky action blocked" / Access is denied

A newly built unsigned `witchat.exe` in OneDrive/Desktop looks like ransomware to Microsoft Defender **Attack surface reduction**:

- Rule: **Use advanced protection against ransomware** (`C1DB55AB-C21A-4637-BB3F-A12568109D35`)
- Symptom: `Program 'witchat.exe' failed to run: Access is denied` from PowerShell
- Toast: *Your administrator has blocked this action* / *App or process blocked: powershell.exe*

This is Defender, not a broken binary.

1. In the Defender toast or **Windows Security → Protection history**, open the event and choose **Actions → Allow on device** (or Allow).
2. Do **not** start it from a PowerShell session inside an IDE (Defender can treat that host as the blocked process). In File Explorer open `dist\windows` and double-click `witchat.exe` (or `run-witchat.cmd`).
3. If it still dies: **Windows Security → Virus & threat protection → Manage settings → Exclusions → Add folder**  
   the folder that holds `witchat.exe`, e.g. `%USERPROFILE%\Downloads\witchat-windows`
4. Optional (admin PowerShell, from that folder):  
   `Add-MpPreference -AttackSurfaceReductionOnlyExclusions (Get-Location).Path`

Do not turn ransomware protection off globally. Each `dist\windows\witchat.exe` ships with `witchat.exe.sha256` and `witchat.exe.json` (git rev, target, build time). Compare with `Get-FileHash dist\windows\witchat.exe` before allowing it. A per-user Setup installer is not shipped.

## Tor (opt-in)

witchat does not bundle Tor. Settings > **TOR** routes every relay connection (private messages over the internet, location channels, sampling, presence) through a SOCKS5 proxy already running on this PC. Host names are sent to the proxy unresolved, so DNS goes through Tor too. If the proxy stops answering, relay traffic **pauses** instead of going around Tor. Bluetooth `#mesh` is unchanged.

Use **one** of the two programs below. Do not point witchat at both at once.

### In witchat (same for both)

1. Start Tor Browser **or** confirm the tor service is running (steps below).
2. Settings > **TOR** > turn on **route relays through tor**.
3. Set the proxy and click **use**:
   - Tor Browser: `127.0.0.1:9150`
   - Expert Bundle / tor service: `127.0.0.1:9050`
4. Wait for the green line: relays see Tor's address, not yours. Orange means nothing is answering yet; leave Tor Browser open, or wait until the service has bootstrapped.

Location presence (so phones count you in `[N people]` without a post) is sent only while this line is green and you have a location fix.
### Tor Browser (simplest)

1. Download only from [https://www.torproject.org/download/](https://www.torproject.org/download/) (64-bit / `x86_64` on a normal PC). Not the Microsoft Store, not a random installer.
2. Install, open **Tor Browser**, and wait until it says it is connected.
3. Leave the window open (minimize is fine; quit is not). That process is the proxy on port 9150.
4. In witchat use `127.0.0.1:9150` as above.

You do not need to browse the web in Tor Browser for witchat. Closing it turns the status orange and pauses relay traffic.

### Tor Expert Bundle (background service)

This is the standalone `tor.exe`, not a browser. It listens on **9050**, can start with Windows, and needs no window. Download it from [https://www.torproject.org/download/tor/](https://www.torproject.org/download/tor/) (**Windows x86_64** Expert Bundle). Keep the `.tar.gz` out of this repo.

The Expert Bundle's `tor.exe` is **not** Authenticode-signed. `Get-AuthenticodeSignature` reporting `NotSigned` is expected. Check the archive against Tor's hash list instead (example for 15.0.25; use the matching folder for a newer file):

```
(Invoke-WebRequest -UseBasicParsing 'https://dist.torproject.org/torbrowser/15.0.25/sha256sums-signed-build.txt').Content
Get-FileHash $env:USERPROFILE\Downloads\tor-expert-bundle-windows-x86_64-15.0.25.tar.gz
```

The SHA-256 lines must match. Optional: also verify that hash list with Tor's PGP key ([how to verify a download](https://support.torproject.org/tbb/how-to-verify-signature/)).

Unpack and install as a service. Run **PowerShell as administrator**, one line at a time:

1. Unpack:

```
New-Item -ItemType Directory -Force 'C:\Program Files\Tor','C:\ProgramData\Tor' | Out-Null; tar -xzf "$env:USERPROFILE\Downloads\tor-expert-bundle-windows-x86_64-15.0.25.tar.gz" -C 'C:\Program Files\Tor'; Get-Item 'C:\Program Files\Tor\tor\tor.exe'
```

2. Settings file (localhost only, port 9050):

```
Set-Content -Encoding ascii 'C:\ProgramData\Tor\torrc' 'SocksPort 127.0.0.1:9050','DataDirectory C:\ProgramData\Tor\data','Log notice file C:\ProgramData\Tor\notice.log'; Get-Content 'C:\ProgramData\Tor\torrc'
```

3. Let the service account write its data:

```
icacls 'C:\ProgramData\Tor' /grant '*S-1-5-19:(OI)(CI)M'
```

4. Install and start:

```
& 'C:\Program Files\Tor\tor\tor.exe' --service install -options -f 'C:\ProgramData\Tor\torrc'
```

5. After about 30 s:

```
Get-Service tor | Format-Table Status, StartType; Get-Content 'C:\ProgramData\Tor\notice.log' -Tail 3
```

Expect `Running`, `Automatic`, and `Bootstrapped 100% (done)`. Then in witchat use `127.0.0.1:9050`. You can quit Tor Browser; the two do not clash if they stay on 9150 and 9050, but witchat only uses the address you set.

The Expert Bundle does not update itself. To replace it: `--service stop`, unpack the new archive over `C:\Program Files\Tor`, `--service start`. To remove: `--service remove`.

### Disconnect

Two different things: turn Tor **off in witchat** (relays go direct again), or stop the Tor **program**. Quitting the program while witchat still has TOR on does **not** go direct; relay traffic pauses until the proxy answers or you toggle TOR off.

**In witchat (stop using Tor):**

1. Settings > **TOR** > turn **route relays through tor** off.
2. The warning returns: relays can see this PC's IP. Presence stops. Location channels and Nostr DMs reconnect without the proxy.
3. Bluetooth `#mesh` is unchanged.

**Tor Browser:** File > Quit (or close the window). That stops port 9150. Uninstall from Settings > Apps if you do not want it on the PC.

**Expert Bundle service** (administrator PowerShell):

```
& 'C:\Program Files\Tor\tor\tor.exe' --service stop
```

That stops port 9050 until the next boot (`StartType` stays Automatic). To prevent it coming back:

```
& 'C:\Program Files\Tor\tor\tor.exe' --service remove
```

Then you can delete `C:\Program Files\Tor` and `C:\ProgramData\Tor` if you want the files gone.

## Identity

Noise/Ed25519 keys live in `%APPDATA%\witchat\witchat\data\identity.json` (Roaming, not Local), encrypted with DPAPI for the current Windows user (`"protection": "dpapi"`). Logs stay under `%LOCALAPPDATA%\witchat\witchat\data\logs` and the filename uses UTC (`witchat.yyyy-mm-dd.log`). Another account, or a copy of the file on another PC, cannot decrypt it; software running as you can. A plaintext file from an older witchat is encrypted on first start. Favorites, verification, contacts and joined channels sit beside it in `social.sealed`, encrypted the same way; settings stay readable in `social.json`. Panic wipe replaces all of them.

## Headless relay

`witchat.exe --relay` relays for the mesh in a console window with no GUI, for a PC left on. It is its own node: identity, settings and logs live in a `relay` folder (`%APPDATA%\witchat\witchat\data\relay`, logs under `%LOCALAPPDATA%\witchat\witchat\data\relay\logs`), so private messages to your own identity never land on a process nobody reads. While on mains power it keeps Windows from idle sleep (`powercfg /requests` in an admin prompt lists it); `--allow-sleep` turns that off. Every 15 minutes it logs what it did for the mesh, as counts. Ctrl-C stops it.

Bluetooth needs your signed-in session, so start it at logon rather than as a service:

1. `schtasks /Create /TN "witchat relay" /TR "\"C:\path\to\witchat.exe\" --relay" /SC ONLOGON /RL LIMITED`
2. `schtasks /Query /TN "witchat relay"` (check it is listed)
3. `schtasks /Delete /TN "witchat relay" /F` (remove it)

The relay reaches Nostr relays directly unless `relay\social.json` has a `tor_proxy` (copy the field from your main `social.json`). Run the relay or the GUI, not both: they share the Bluetooth adapter, so whichever starts second exits and names the one already running.

## Logs

Witchat writes rotating daily files (keeps 7) to:

`%LOCALAPPDATA%\witchat\witchat\data\logs\`

Example: `%LOCALAPPDATA%\witchat\witchat\data\logs\witchat.2026-10-15.log` (the date is UTC)

The GUI status line shows `logs <path>` at startup. Default file level is `debug` for witchat crates. Override with `RUST_LOG=witchat_ble=trace`. Do not commit log files; they may include BLE addresses (not identity secrets).

## Build

```
powershell -File scripts\build-windows.ps1
```

Binary: `dist\windows\witchat.exe`.
