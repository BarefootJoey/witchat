# Security

## Reporting a vulnerability

Please report privately through GitHub: **[Security > Report a vulnerability](https://github.com/BarefootJoey/witchat/security/advisories/new)**. Do not open a public issue for anything that could put users at risk.

Include what you found, how to reproduce it, and what an attacker gains. A rough proof of concept is welcome. Expect an acknowledgment within a week. Fixes ship in a new release with credit, unless you ask to stay anonymous.

If the bug is in the bitchat protocol itself (it affects the official apps too), tell us and we will coordinate with [permissionlesstech/bitchat](https://github.com/permissionlesstech/bitchat).

## Supported versions

Only the latest release gets fixes. Witchat is pre-1.0; there are no long-term branches.

## What witchat protects

| Asset | Protection |
|---|---|
| Private messages over Bluetooth | `Noise_XX_25519_ChaChaPoly_SHA256` with the official explicit-nonce framing and a 1024-message replay window. Relaying peers see only ciphertext. |
| Private messages over Nostr | iOS-compatible gift wraps (kinds 1059/13/14, XChaCha20-Poly1305). Location-channel private messages use a per-cell key that is not linked to your mesh identity. |
| Identity keys | Windows: DPAPI for your user account. Linux: a wrap key in the session Secret Service, then ChaCha20-Poly1305 around `identity.json`. If no keyring answers, an owner-only file (`0600`), not encrypted. |
| Unsent private messages | An outbox sealed the same way as the identity, dropped after 24 h or on delivery. If no keystore answers, memory only; message text is never written in the clear. |
| Mail carried by couriers | As in iOS: a message for someone out of reach is sealed with `Noise_X_25519_ChaChaPoly_SHA256`, to one of their signed one-time prekeys when we hold a fresh bundle (else to their static key), and handed to up to three nearby mutual favorites or verified peers. A mesh message waiting for them, or a Nostr message every relay refused, is what gets that copy. A relay that accepted it does not. Couriers see an opaque blob and a recipient tag that rotates daily and can only be matched by someone who already knows the recipient's key. Witchat carries mail only from mutual favorites (5 each) or peers whose announce signature verifies (2 each, 20 total), at most 40 envelopes for 24 h. Envelopes from blocked senders are dropped unopened. |
| Who you talk to (verification) | Fingerprints and QR verification. A QR only verifies someone once they answer a challenge signed with the key in the code. Witchat also warns when a session's signing key contradicts the announce. |
| Your IP address (opt-in) | With Tor on, every relay connection, including the daily relay-directory download, goes through your local Tor SOCKS proxy. If Tor is down, relays wait; witchat never falls back to a direct connection. |
| Incoming files | MIME allow-list, magic-byte check, 1 MiB cap. Unsigned broadcast files are dropped and not relayed. Files from blocked peers never touch disk. |
| Notifications | Toasts name the sender and chat, never the message text. The panic wipe clears them from the OS notification history. |
| Panic wipe | Replaces the identity and deletes favorites, contacts, history, outbox, carried courier mail, the list of peers this PC has met, prekeys and cached bundles, ledgers, the cached relay directory and channel memberships. Tor stays on so a wipe never exposes your IP. The headless relay's separate `relay` folder (`witchat --relay`) is not touched; delete that folder to remove it. |

## What witchat does not protect

- **Public chats are public.** `#mesh` and location channels are signed, not encrypted. Anyone nearby, or any relay, can read them.
- **Your device can be followed by radio.** As in the official apps, announces carry a stable peer id, so a nearby listener can recognize the same device over time.
- **Metadata.** Relays see when you connect and which events you fetch. Without Tor they also see your IP address. Bluetooth neighbors see that you are present and roughly how much traffic you send.
- **A compromised PC.** Malware running as your user can read DPAPI-protected data, the session keyring, and the screen. On Linux, if no Secret Service is running, the identity file is plaintext.
- **Unsigned binaries.** Releases are not Authenticode-signed yet. Check the `.sha256` and the provenance `.json` against the release page before running a download ([docs/WINDOWS.md](docs/WINDOWS.md#unsigned-download-smartscreen-and-defender)). A later publisher signature names who shipped the exe; it does not replace the hash check, and it does not clear the Defender ransomware-folder block. Copying the folder to a USB stick can skip SmartScreen and is still not a verified build.
- **Some couriered mail has no forward secrecy.** Mail sealed to a one-time prekey cannot be opened once the recipient deletes that key (48 h after first use). Without a fresh bundle from the recipient, mail is sealed to their long-term key, so a later leak of that key opens any such envelope someone captured. Witchat publishes prekeys only when an OS keystore can keep them; without one, phones seal to witchat's long-term key. Live sessions are used whenever the person is in range.
- **Bugs in the official protocol.** Witchat implements the protocol as published and inherits its design limits.
- **No anonymity claim.** Timing and size of Bluetooth traffic stay visible, and a listener in several places can correlate them. Witchat has not had an independent security review.

## What stays on disk

On Windows, roaming files (identity, settings, ledgers, history) are in `%APPDATA%\witchat\witchat\data` and machine-local ones (logs, outbox, relay directory, toast icon) in `%LOCALAPPDATA%\witchat\witchat\data`. On Linux everything is in `~/.local/share/witchat` (or `$XDG_DATA_HOME/witchat`). Windows notifications also register the app name and icon under `HKCU\Software\Classes\AppUserModelId\witchat.desktop`.

| File | Contents | Protection |
|---|---|---|
| `identity.json` | Noise and signing keys, nickname | DPAPI (Windows) / Secret Service wrap (Linux) / `0600` plaintext if no keyring |
| `social.sealed` | Favorites, verified and blocked fingerprints, contacts' `npub`s, bookmarks, relays | Same seal as identity. If the keyring cannot open it, it is left untouched and the session runs without it |
| `social.json` | Settings (Tor proxy, notifications, close behavior, logging, location, history). Without a keystore, also everything listed for `social.sealed` | Plain JSON (`0600` on Linux). A plain file from an older witchat moves into `social.sealed` on first start |
| `dm_outbox.sealed` | Unacknowledged outgoing private messages, at most 24 h | Same seal as identity; omitted when no keystore |
| `courier.sealed` | Envelopes carried for others: opaque ciphertext, recipient tags, depositors' public keys, at most 24 h | Same seal as identity; omitted when no keystore |
| `peer_keys.sealed` | Peer ids and Noise public keys of up to 1024 peers heard, so DMs to absent peers can be sealed for couriers (says who this PC has met) | Same seal as identity; omitted when no keystore; removed by panic wipe |
| `prekeys.sealed` | Our one-time prekey private keys (used ones for 48 h) and other peers' public prekey bundles, which show whom we met | Same seal as identity; omitted when no keystore (then no prekeys are published) |
| `nostr_dm_ledger.json`, `media_ledger.json` | Ids of received messages and files (no names, no content), 25 h / 7 days | Plain JSON |
| `relay_directory.csv` | Public relay list | Plain |
| `history.json` | `#mesh` and location lines, only after `/history on` | Plain JSON |
| `logs/witchat.*.log` | Connection events, peer ids, some nicknames and Bluetooth device names, relay addresses, and a one-line health summary a minute. **verbose logging** (debug tab) adds per-packet and per-device detail. Never message text, public or private, keys or tokens | Plain; rotated daily, 7 kept |
| `Downloads\witchat\` | Files you received | Plain files |

Private message history is never saved.

The diagnostics report (debug tab, or `witchat --diag`) is built for public bug reports: it leaves out message text, keys and nicknames, shortens peer ids and fingerprints to four characters and Bluetooth addresses to their last two bytes, and includes recent warnings from the log with the same shortening. Relay addresses and relay error messages are kept. Nothing is sent anywhere; it only goes to your clipboard or terminal.

## Scope notes for researchers

Interesting targets: packet and TLV decoders (`crates/witchat-protocol`), fragment reassembly, the Noise session and replay window (`crates/witchat-crypto`, `crates/witchat-core/src/private_chat`), gift-wrap parsing (`crates/witchat-nostr`), file validation, and anything that makes witchat relay, store or answer more than it receives (amplification). Ping answers are capped at 5 per 10 s for that reason, and gossip-sync answers (fragment replays included) at 8 per 30 s per neighbor.

Out of scope: denial of service by jamming Bluetooth, attacks that need the user's Windows account, and limits already listed above.
