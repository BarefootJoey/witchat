# Linux relay operations — verified observations, working configurations, and open questions

Field documentation from running `witchat --relay` against iOS phones across three board classes, plus a procedure for diagnosing relay/Bluetooth problems without destroying working configurations.

# CM5 (piunora): witchat relay and Bluetooth operations

**Baseline recorded November 8; live-state snapshot verified November 8 ~14:05 PST against the running station.** This document distinguishes the deployed configuration from the recommended template. Verify live state before making changes.

## Live state (verified 2026-11-08 ~21:55–22:03 UTC)

| Item | Verified value |
|---|---|
| `witchat-relay.service` | active; build `witchat 0.1.0 (7badfb3ef724)` on `linux/aarch64` |
| Dep build lineage | **`7badfb3`** — piunora repo `main` HEAD (message: "diagnostics: per-check announce trust booleans"). The SHA `e87853a` quoted earlier for the same change is the openpi-side clone's SHA; do not mix the two when citing builds. |
| Dep identity fingerprint | `00da626a96bf64bf` (confirmed 1,700+ times in today's relay log) |
| Dep nickname | `CollisRelayStation1` |
| Controller | `2C:CF:67:F0:3A:F2` (hci0, UART, BCM…); **Name: `piunora`**, **Alias: `CollisRelayStation5`** |
| Roles | **Central-only.** witchat's peripheral path fails at startup (`central-only: advertise: … Failed to register advertisement`, most recent 21:56:21) |
| Generic beacon | `bt-beacon-s1.service` active, but its `advertise on` is **currently failing too** — repeated `Failed to register` in `/tmp/bt-beacon.log` while the 25 s reassert loop runs; `btmgmt info` shows no `advertising` bit. `[O]` the plain generic beacon that registered on Nov 5–7 now fails as well — do not assume the generic path is immune to the controller's registration failure; treat "which advertise payload fails, when" as a new open question. |
| Current sessions | Peers `db7b50a72cdb45d3` (nick `snar`) and `c81f1e013ab28597` (`anon4437K`) visible via announces; health `secured=1`–`2` with traffic both directions |
| Adapter settings file | `/var/lib/bluetooth/2C:CF:67:F0:3A:F2/settings` (Discoverable/Pairable true, timeouts 0) |

A witchat service restart at 21:56:21 followed the beacon-off test window; the beacon holder kept its agent registered but lost (and could not re-register) its advertisement. Current state therefore = agent up, **no advertisement on air**, central-only relay healthy.

## Architecture

The working witchat connection path observed on this station is:

**CM5 scans → connects outward to the iPhone’s GATT server → subscribes → exchanges witchat packets.**

A separate `bluetoothctl` session provides the default pairing agent and optionally broadcasts a generic Bluetooth advertisement.

- The generic advertisement contains the adapter name and flags, without the bitchat service UUID.
- Visibility in iPhone Bluetooth Settings does not establish visibility or message delivery inside bitchat.
- Witchat’s own advertisement registration has failed with **MGMT Invalid Parameters (`0x0d`)**. This does not, by itself, establish a controller-firmware defect.
- Local GATT-server registration remains a separate verification question. Absence from `busctl tree org.bluez` alone is insufficient evidence.

## Components

### 1. `witchat-relay.service`

```ini
ExecStart=%h/.local/bin/witchat --relay --nick CollisRelayStation1
```

Recorded build:

- Native aarch64.
- Commit lineage `905f17d` → `e87853a`.
- Local patches: six-hour public-message retention, disk persistence, and announce-trust instrumentation.
- Outgoing BLE connections have established Noise sessions.
- Nostr relay connections were reported healthy; verify their intended function separately from local Bluetooth relaying.

Record the exact executable hash, source revision, local patches, and service definition after each deployment.

Verified today: unit `ExecStart=%h/.local/bin/witchat --relay --nick CollisRelayStation1`; build `witchat 0.1.0 (7badfb3ef724)`; fingerprint `00da626a96bf64bf`.

### 2. `bt-beacon-s1.service`

Existing script: `bt-beacon-s1.sh`.

The deployed design uses one long-lived `bluetoothctl` session, receiving commands through `/tmp/beacon-in`:

```text
agent NoInputNoOutput
default-agent
advertise on
pairable on
discoverable on
```

**Agent capability is intended configuration, not confirmed outcome.** Captured pairing responses have still presented `KeyboardDisplay (0x04)`. Identify the selected agent and transmitted capability before claiming Just Works works or is unsupported.

Application-specific agents and explicit default-agent selection govern agent handling. Do not assume “last registered wins” or that one session makes all capability problems impossible.

The existing script also includes:

- Flag reassertion every 25 seconds.
- Hourly session replacement.
- Controller power-cycling following advertisement-registration failure.

These are historical recovery behaviors, **not requirements to copy into new deployments**. Radio resets interrupt witchat’s connections; session replacement interrupts agent and advertisement availability.

### 3. Recorded BlueZ settings

`/etc/bluetooth/main.conf`, under `[General]`:

```ini
DiscoverableTimeout=0
PairableTimeout=0
```

Recorded backup: `main.conf.bak-20261107`.

Adapter settings file:

```text
/var/lib/bluetooth/2C:CF:67:F0:3A:F2/settings
```

Recorded state:

```ini
Discoverable=true
Pairable=true
DiscoverableTimeout=0
PairableTimeout=0
```

Zero timeouts disable automatic expiry; they do not alone prove the enabled states survive every restart. Verify after reboot. Stop bluetoothd before deliberately editing its stored adapter settings, and preserve a backup.

### 4. Identity

- Hostname: `piunora`.
- Witchat nickname: `CollisRelayStation1`.
- Recorded fingerprint: `00da626a96bf64bf`.
- Recorded Bluetooth alias: `CollisRelayStation5`.

The differing names can confuse observations. Align them deliberately when appropriate.

**Changing the Bluetooth alias does not change controller identity or clear bonds.** Preserve witchat identity and Bluetooth bonds during ordinary troubleshooting. Credentials belong in the secrets store, not this document.

## Template script starting point

Preserve the existing script. Create a station-specific copy when adapting the setup.

The following is an **experimental starter**, not a verified replacement. It keeps stdin open, uses a private runtime directory, and avoids automatic radio resets or periodic restarts.

```bash
#!/usr/bin/env bash
set -euo pipefail
umask 077

: "${XDG_RUNTIME_DIR:?Run in the station user's runtime environment}"
BT_ADAPTER="${BT_ADAPTER:?Set the adapter address accepted by bluetoothctl select}"
ENABLE_BEACON="${ENABLE_BEACON:-0}"

runtime_dir="$(mktemp -d "$XDG_RUNTIME_DIR/witchat-bt.XXXXXX")"
fifo="$runtime_dir/input"
bt_pid=""

cleanup() {
    trap - EXIT INT TERM
    exec 3>&- || true
    if [[ -n "$bt_pid" ]]; then
        kill "$bt_pid" 2>/dev/null || true
        wait "$bt_pid" 2>/dev/null || true
    fi
    rm -f -- "$fifo"
    rmdir -- "$runtime_dir" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkfifo "$fifo"
exec 3<>"$fifo"

# stdout/stderr go to the service journal.
bluetoothctl <"$fifo" 3>&- &
bt_pid=$!

printf 'select %s\n' "$BT_ADAPTER" >&3
printf 'agent NoInputNoOutput\ndefault-agent\n' >&3
printf 'pairable on\ndiscoverable on\n' >&3

if [[ "$ENABLE_BEACON" == "1" ]]; then
    printf 'advertise on\n' >&3
fi

wait "$bt_pid"
```

Before deployment:

- Verify command completion and agent-registration success; process survival alone is insufficient.
- Add tested readiness/error handling appropriate to the installed BlueZ version.
- Keep generic advertising optional and independently switchable.
- Do not add unattended `sudo`, recurring resets, or identity/bond deletion.
- Record rollback steps and validate the station-specific service.

## Latest beacon-off test

Advertising was disabled inside the existing session while preserving the agent, bonds, identity, and other settings.

Station-side observations included outgoing connections, established Noise sessions, increasing traffic counters, and recurring pairing/disconnection activity.

**Supported finding:** outgoing connections and Noise sessions were possible without the generic beacon.

**Still unverified in that window:** continuous foreground operation, actual in-app visibility, and delivery of identifiable messages. Do not infer these from aggregate counters.

## Operating workflow

1. Read this runbook and record live configuration before changing it.
2. Identify the failing layer: discovery, GATT, Bluetooth pairing, witchat protocol, or actual delivery.
3. Capture one timestamped connection, including direction and connection handle.
4. Change one variable and preserve a rollback.
5. Verify in-app visibility, identifiable message delivery, idle stability, and reconnection.
6. Verify cold-boot recovery before declaring the setup persistent.
7. Update this runbook in the same session. Separate observations, hypotheses, and confirmed fixes.

Treat “accessory would like to open bitchat” as an app-opening alert, not evidence of Bluetooth pairing. Numeric-comparison prompts and SMP packets must be recorded separately.


> **How to read this:** a successful command is not proof of successful pairing, message delivery, or persistent operation. Each claim below is tagged:
> - **[O]** = direct observation from relay logs or btmon captures (what we saw)
> - **[W]** = working configuration (reproduced more than once on the named board)
> - **[?]** = hypothesis or unresolved — do not treat as established
>
> Board history, raw interpretation of captures, and per-experiment details are deliberately **not** in this file; they live in upstream issues #1 and #2 with their evidence. Consult those before drawing conclusions the tables here do not answer.

Environment backdrop (record per experiment — do not copy across experiments): Raspberry Pi OS Trixie; BlueZ 5.82; kernel versions seen during this work: **6.12.75+rpt-rpi-v8 (CM4/CM5 tests, Nov 5–8)** and **6.18.50+rpt-rpi-v7 (Zero 2 W captures, Nov 4)**; witchat commit lineage `905f17d → e87853a` (local patches: 6 h public-message retention, packet persistence, announce-trust instrumentation; fingerprint changes when `identity.json` is regenerated).

---

## 1. The role planes (verify which one you are in before diagnosing)

Two independent planes fail separately. Identify the failing layer first; most dead ends came from treating them as one thing.

| Plane | What it does | Observable when alive |
|---|---|---|
| **Central (client)** | relay connects *out* to the phone's GATT server, subscribes, carries mesh traffic, establishes the witchat Noise session | `advertisement matched` → `GATT connect` → `connected` → `subscribed to notifications` → `link bound to peer`; then `noise: session established`; health shows `secured=1` **[O]** |
| **Peripheral (server)** | relay advertises and serves the bitchat GATT service so phones can connect *in* | `phone subscribed to our GATT server`, `phone writing to our GATT server`; health `ads=1` |

[O] The planes failed independently on every board tested. A working central path carried secured sessions and relay traffic on a board whose peripheral path never registered.

[W] The central path established full sessions (`noise: session established`, `secured=1`, public traffic relayed) on CM4 and CM5.

[?] Whether the central path is end-to-end functional on Pi Zero 2 W is **unconfirmed** — transport attached but no application session completed in our tests (see issue #2 for the capture evidence; the board-level cause attribution is also unresolved).

## 2. Board results (per-experiment, not universal)

Record your own versions/firmware/role/agent config per experiment; ours are listed so others can compare, not as prescriptions.

| Board / controller | Peripheral path | Central path | Pairing behavior observed | Config notes for that experiment |
|---|---|---|---|---|
| CM4, wireless variant, BCM4345C0, k6.12.75+rpt-rpi-v8 | [W] registered first try | [W] sessions in seconds, `secured=1` | [O] Just Works (no PIN) on first pairing attempt | Required `dtparam=uart0=on` + `enable_uart=1` in `/boot/firmware/config.txt` on *our* carrier/setup — [O] without it the UART BT controller failed at boot (`BCM: Reset failed (-110)`). Whether other CM4 carrier/bt-UART setups need the same is untested. |
| CM5, on-board UART BT, k6.12.75+rpt-rpi-v8 | [O] advertisement registration fails every attempt, incl. after daemon restart and cold reboot; btmon: Add-Extended-Advertising-Parameters → *Success (0x00)*, followed by Add-Extended-Advertising-Data → **MGMT Invalid Parameters (0x0d)**. Where exactly the rejection happens (controller firmware vs bluetoothd vs policy) is **unresolved** — the MGMT status alone does not locate it. | [W] full central sessions, `secured=1` | [O] every captured pairing exchange answered with responder `IO capability: KeyboardDisplay (0x04)` **despite a `NoInputNoOutput` agent being registered** → numeric-compare/PIN presented to the user. Cause unresolved — identify the actually-selected agent (application-specific vs default) before assuming any mechanism. | Relay runs healthy central-only; its own peripheral path must be considered dead. A generic name-only beacon (see §6) makes the box visible in the phone's Bluetooth settings but does **not** create a bitchat peer. |
| Pi Zero 2 W, BCM43430, k6.18.50+rpt-rpi-v7 | [O] advertisement + GATT registration succeeded | [?] transport attached (connect/subscribe/bind) but no application session ever completed in our tests (issue #2); pairing exchange stalled before key exchange | [O] pairing prompts appeared; exchanges aborted early (see issue #2 for the frame-level record; controller-firmware attribution there is a hypothesis, not a finding) | Recoverable with a USB BT dongle on the same board ([W] dongle enumerated + scanned; full pairing on dongle silicon not completed — mark as incomplete) |

Cross-board sanity notes:
- [O] The same SD card moved between boards kept witchat identity/nick/services; the BLE address changed (expected — new controller).
- [O] Renaming the adapter alias (`bluetoothctl system-alias …`) changes only the advertised name — it is not a device identity reset and does not clear bonds.

## 3. Advertising failures (observations + how to distinguish layers)

[O] Failure signature: relay logs `BLE role status=central-only: advertise: Bluetooth operation failed: Failed to register advertisement` repeating, while central-only operation continues normally.

[O] btmon during such registration shows the MGMT two-step split: parameters command succeeds, data command returns Invalid Parameters (0x0d).

[O] On the CM5 this survived daemon restarts and a cold reboot — an operational-recovery problem, not a config artifact. Root layer unresolved.

[Dx — verification limits] `busctl tree org.bluez` shows objects **registered under org.bluez**. Absence of local GATT objects there is consistent with an unregistered GATT application, but is **not sufficient proof**: applications export their own objects over D-Bus. To verify registration results authoritatively, check the application's registration call/result (e.g., bluer's errors in the relay log) and the application's exported object hierarchy.

[O] A plain `bluetoothctl advertise on` (no service UUIDs) *can* register on the same controller that rejects witchat's payload, making the box visible in the phone's Bluetooth **settings** list. Limitations, verified: name + generic flags only (no UUID), and it does **not** cause the relay to appear as a peer inside the bitchat app — in-app listing followed witchat session establishment, not the beacon. The generic beacon is therefore **optional**; relay operation never required it.

## 4. Agents (verify what actually answers, never assume)

[O] Every `bluetoothctl` session we started registered an agent implicitly. Which agent answers a given pairing exchange depends on BlueZ's agent-selection rules (application-specific agents for actions triggered by that application; explicit `default-agent` as fallback). We did **not** determine which agent answered a specific exchange where the responder presented KeyboardDisplay — the selection question is open.

[Dx] Do not diagnose from agent registrations alone. Capture the exchange: btmon during the pair attempt, `grep -aE "Pairing (Request|Response)" -A2` shows **both** capabilities. In our captured exchanges the responder presented `IO capability: KeyboardDisplay (0x04)` while a NoInputNoOutput agent was registered — how that selection resolved is unresolved.

[SMP spec note] I/O capability values in SMP fields: DisplayOnly 0x00, Just Works 0x01, KeyboardOnly 0x02, NoInputNoOutput **0x03**, KeyboardDisplay 0x04.

[O] Pairing-method outcome on the CM5 stack: numeric-compare (PIN screen) was presented in every attempt across multiple configurations, including with an explicit NoInputNoOutput agent registered and only one bluetoothctl session alive. Just Works was **not achieved** on this board in our attempts. On the CM4, the first pairing attempt was Just Works.

[O] One documented teardown sequence: a pairing offer left unanswered times out (~60 s), iOS sends `Pairing Failed (0x0c, Numeric comparison failed)`, and BlueZ tears down the whole link with `Disconnect, Reason: Authentication Failure (0x05)` (btmon-captured). The relay log shows the aftermath as `BLE link went silent; dropping it`.

**[?] Do not treat "a pending pairing offer exists" as a universal explanation for that watchdog line.** In our captures the sequence above was observed — but log correlation alone does not establish that *every* `went silent` drop had a matched pairing-failure event, nor that resolving pairing offers prevents drops generally. For each drop you diagnose, require a matched event sequence: btmon/journal evidence pairing that specific connection's teardown to a specific SMP failure (or showing its absence). Completing a pairing once (durable bond), or clearing a stale offer (Forget This Device + BT toggle), stopped *observed instances* of the cycle — a recorded intervention result, not a proven universal fix.

## 5. Diagnostics procedure (one variable at a time, always with a rollback)

1. **Read the station's current operations runbook** (the working configuration, last known good state, and known quirks for that box).
2. **Record the running configuration** before touching anything: service states, adapter flags, alias, identity fingerprint (`grep identity relay/logs/<today>.log | tail -1`), agent processes, kernel/BlueZ versions. If the runbook disagrees with the live state, reconcile the runbook — do not "fix" the live state to match stale notes.
3. **Identify connection direction and failing layer** (central vs peripheral; BT transport vs app session vs pairing; §1 plane table + §3 registration checks).
4. **Capture one timestamped attempt** end to end: `sudo btmon -w /tmp/<name>.cap` during the reproduce, plus the relay log tail. btmon decides agent/capability and teardown questions logs can't.
5. **Change one variable, with a rollback you've written down first.** One flag, one identity action, one service restart per step.
6. **Verify at the app level** after each change: does the peer appear in the phone's app (not just Bluetooth settings)? Does a public message actually forward (relayed count increasing)? Does the connection hold >2 min? Does reconnect work after BT toggle?
7. **Update the runbook with what you did, evidence, and remaining uncertainty.** If a hypothesis hardened into a finding, move it (with its capture reference) into the issues. If it remains unresolved, write it down as unresolved — that note is what prevents the next operator from repeating your dead end.

Things that are **out of ordinary troubleshooting** (separate, deliberate reset procedures):
- **Regenerating witchat identity** (`identity.json` deletion): changes the node fingerprint and trust relationships; requires backup + rollback plan; and [O] it does **not** clear Bluetooth bonds (BlueZ keeps `dev_*` records independently).
- **Purging BlueZ bond records / `btmgmt` power-cycles**: both disconnect live links and can leave the radio in states that need re-verification. Do them when a bond/registration question is actually open, not as a reflex.

## 6. Optional name-only beacon (not required for relay operation)

[O] Where the peripheral path is dead (CM5-class), some operators still want the box visible in phone Bluetooth settings for debugging. The minimal pattern is **one** `bluetoothctl` session, deliberately also holding the agent so no second session can register a different-capability agent:

- holds stdin open via FIFO (a bare `bluetoothctl agent …` exits on stdin EOF), asserts `agent NoInputNoOutput` + `default-agent` + `advertise on` + flags
- on registration failure, power-cycles the controller once and re-asserts **bounded** — power-cycling disconnects witchat's live links, so it must never run unconditionally
- re-asserts flags on a heartbeat because bluetoothd resets `Discoverable`/`Pairable` across restarts

**Do not blindly copy an ad-hoc script.** Our first holder version had real problems (fixed /tmp paths, hardcoded UID, no cleanup trap, sudo inside an unattended service, error-check racing async output, blocking FIFO writes, hourly respawn gaps, unconditional power-cycles dropping live links). If you want the holder as a service, write it for your station: private runtime dir, journal logging, explicit readiness check, cleanup traps, bounded recovery — and run it through the §5 procedure. Treat it as optional tooling, not a required component.

**[W] Beacon/relay coexistence:** the beacon served name-only visibility in Bluetooth settings; in-app peer listing followed witchat session establishment.

**[O] Beacon-off experiment (2026-11-08):** advertising was disabled *inside* the live holder session (agent preserved; controller `advertising` state verified off; bonds/identities unchanged). Station-side observations with advertising off:
- Outgoing phone connections and established Noise sessions continued (both peer fingerprints observed; `secured=2` at window close).
- Traffic counters increased (relayed 436 → 447 during the window).
- Pairing-related activity and silence-watchdog disconnects were **still observed** — disabling the beacon did not eliminate either behavior.
- A captured local pairing response presented `KeyboardDisplay (0x04)` despite the intended NoInputNoOutput configuration. **Agent selection remains unresolved; associate this response with its exact connection (handle/frame sequence) before diagnosing it.**

Supported conclusions: the generic beacon is not required for the observed outgoing connections and Noise sessions; disabling it did not eliminate the observed pairing/disconnection behavior. These observations do **not** establish the underlying cause of either behavior. Do not restore the generic beacon merely to improve Bluetooth Settings visibility — restore it only for a stated purpose, recording the change.

**Incomplete validation:** the operator did not confirm continuous foreground operation, in-app relay visibility, or delivery of identifiable messages during the window. Do not infer those from `secured=` counts or aggregate traffic counters.

**Next test:** leave advertising disabled and the agent running; operator keeps bitchat foregrounded with the screen awake for five minutes; confirm relay visibility in-app; exchange identifiable messages between phones; correlate UI observations and message delivery with **timestamped logs and the relevant Bluetooth connection handles**.

## 7. Open items and where the evidence lives

- CM5 peripheral advertisement rejection (MGMT Invalid Parameters 0x0d on data step) — layer unresolved → issue #2 (+ captures)
- Zero 2 W session-establishment failure with transport alive — attribution unresolved → issue #2
- Offline DM relay/spooling → issue #1
- KeyboardDisplay responder despite NoInputNoOutput agent on CM5 — agent-selection mechanism unresolved (this doc, §4)
- Pi Zero 2 W central-path end-to-end status — unconfirmed (see §1)

*This file is field documentation. When your experiments extend or contradict it — especially the board table and pairing-method findings — open an issue with a btmon capture rather than editing conclusions here, so observations stay separated from diagnoses.*
