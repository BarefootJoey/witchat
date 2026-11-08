# Running a witchat headless relay on Linux — field guide (operator skill doc)

Written from a week of real deployments against iOS phones on three board classes (CM4, CM5, Pi Zero 2 W), all on Raspberry Pi OS Trixie / BlueZ 5.82 / kernel 6.12. This covers what the [headless relay](LINUX.md#headless-relay) section starts — and the Bluetooth/agent layer beyond it that you only learn when phones stop behaving. Every claim here has been reproduced with `btmon` traces or relay logs; where a behavior is board-specific we say so.

> Status: operator documentation from the field, not yet maintainer-reviewed. The companion test evidence lives in issues #1 and #2.

---

## 1. The two role planes (know which one you're in)

witchat on Linux has two independent planes, and they fail separately. Diagnose them apart:

| Plane | What it does | How you know it's alive |
|---|---|---|
| **Central (client)** | witchat connects *out* to phones' GATT servers, subscribes, exchanges mesh packets, establishes the witchat Noise session | `advertisement matched / GATT connect / connected / subscribed to notifications / link bound to peer` then `noise: session established`; health line shows `secured=1` |
| **Peripheral (server)** | witchat publishes the bitchat GATT service + advertisement so phones find and connect *in* | `phone subscribed to our GATT server` + `phone writing to our GATT server`; health `ads=1` |

**Key fact:** the planes are independent. A fully working central path (secured sessions, mesh relay) does **not** require the peripheral path, and a dead peripheral path does not impair relaying. On boards where the peripheral path is broken (see §3), relays still function — they're just invisible to phone *scans*, not to phone *apps*.

The central path is untested upstream but is, in practice, the reliable one: it worked end-to-end (Noise sessions, `secured=1`, private + public traffic) on every board we tried.

## 2. Board reality (what each class supports)

Same binary, same BlueZ, three very different outcomes:

| Board | BT chip | Peripheral path (advertise + GATT server) | Central path (sessions from phones) | Pairing behavior toward iOS |
|---|---|---|---|---|
| CM4 (BCM2711, wireless variant, BCM4345C0) | in-kernel UART after `dtparam=uart0=on` | ✅ works first try | ✅ sessions in seconds | Just Works (no PIN) first try |
| CM5 (BCM2712, Pi 500-style UART BT) | on-board | ❌ advertisement registration fails at the controller | ✅ works fully (attach → subscribe → bind → `secured=1`) | numeric-compare/PIN only (see §5) |
| Pi Zero 2 W (BCM43430) | on-board | ✅ advertises, but see #2 for the session-level failure | ✅ works | prompts but pairing stalls (controller/firmware, separate issue) |

Notes:
- The **CM4 needs `dtparam=uart0=on`** (and `enable_uart=1`) in `/boot/firmware/config.txt`; the Zero W exposes its BT UART by default, the CM4 does not. Without it btmon shows `hci0: BCM: Reset failed (-110)` at boot.
- **Same SD card moved between boards** keeps identity/nick/services intact — BLE address changes are expected (new controller).
- The peripheral-path failure on CM5-class silicon is at the kernel/mgmt layer, not a configuration artifact: it survives daemon restarts and cold reboots. btmon signature: `Add Extended Advertising Parameters → Status: Success`, then `Add Extended Advertising Data → Invalid Parameters (0x0d)`.

## 3. Advertising: what fails, what still works, and what's cosmetic

`witchat --relay` publishes its advertisement through BlueZ with its own service UUID. On some boards this registration is rejected:

1. **Symptom:** log line `BLE role status=central-only: advertise: Bluetooth operation failed: Failed to register advertisement` repeating. Relay continues normally as central-only.
2. **btmon fingerprint:** `MGMT Command: Add Extended Advertising (0x0054)` returns *Success*, the data command (`0x0055`) returns *Invalid Parameters (0x0d)*.
3. **Check what's actually registered:** `busctl tree org.bluez | grep hci0` — local GATT services = peripheral alive; only `dev_*` (remote devices) = peripheral path dead.
4. **A plain `bluetoothctl advertise on`** (no service UUIDs) *can* still register on the same controller. That makes the box visible in *phone Bluetooth settings* (name-only beacon — no UUID, no GATT behind it). It does **not** make the relay appear in the *bitchat app*; in-app peer listing follows witchat session establishment. Useful for debugging visibility, but know which layer it feeds.
5. **Unresolvable-pairing-offer teardown (iOS):** if a pairing request toward your relay is left unanswered, iOS's SMP times out (~60 s) and sends `Pairing Failed (0x0c, Numeric comparison failed)`, after which BlueZ tears down the **whole link** (`Disconnect, Reason: Authentication Failure (0x05)`). The relay's log shows this as `BLE link went silent; dropping it`. An otherwise-healthy app session dies with it. Resolution: complete the pairing once (bond survives), or remove the stale pairing offer (Forget This Device on the phone) — see §5.

## 4. Agents: the layer nobody warns you about

Every `bluetoothctl` session you start can register an agent; whichever agent answers the SMP exchange defines the pairing *method* (see bluer's `agent.rs` capability derivation: handlers present → `DisplayYesNo`/`KeyboardDisplay`, none → `NoInputNoOutput`).

**Field consequences:**

- **One agent holder per machine.** Multiple `bluetoothctl` sessions each registering agents is how "my `NoInputNoOutput` agent is registered but iOS still shows a PIN prompt" happens — a second agent with display-capable handlers answered first. Our verified-stable setup uses **one** session holding: `agent NoInputNoOutput` + `default-agent` *plus* the beacon duties (§6) — i.e., agent and beacon in the *same* session, so nothing else can register agents.
- **`agent off` only unregisters that session's own agent.** It will not clear another session's registration, and "which agent is default" is a per-application/default-agent precedence question, not a global "last registered wins" — keep the model straight when debugging.
- **witchat does not register its own agent** (it uses the default agent for pairing actions). What answers is your bluez agent layer + whatever sessions you left running.
- **Method math:** iOS pairs as KeyboardDisplay with MITM. If our responder also presents display-capability (or the phone cached that from an earlier attempt), SMP lands on *numeric comparison — the 6-digit PIN screen*. Just Works (no PIN) requires our responder to present `NoInputNoOutput`. That was achieved first-try on CM4; on CM5's stack the response still presented KeyboardDisplay even with a single NoIO agent — treat "no-PIN pairing on some controllers" as board-dependent until your own btmon shows a `Pairing Response` with `IO capability: NoInputNoOutput (0x01)`.
- **btmon is the arbiter:** capture during the pair attempt — `grep -aE "Pairing (Request|Response)" -A2` shows *both* capabilities. Don't reason from agent registrations alone; watch the exchange.

## 5. The pairing → session stability loop

Observed repeatedly, and the cause of most "relay appears for a minute then vanishes" reports:

```
relay session up (secured) → phone has pending pairing offer for the relay
→ offer unanswered ~60 s → SMP timeout → phone sends Pairing Failed (0x0c)
→ BlueZ tears down the whole link (Authentication Failure 0x05)
→ relay peer disappears from the app → repeat next time phone re-offers
```

**Fixes, in preferential order:**
1. Complete the pairing **once** (bond persists; iOS reconnects silently thereafter — this matches the long-held-session behavior seen on day one).
2. Or flush the stale offer: phone Settings → Bluetooth → forget the relay, toggle Bluetooth, reopen app (fresh attempt; if the responder presents NoInputNoOutput this becomes Just Works).
3. Keep the app foregrounded during the first attach after any identity/bond reset — first `GATT connect` after changes often times out once while iOS settles, then succeeds on retry (~50 s is normal; the relay backs off exponentially, so a relay restart resets the retry cadence).

**Never leave a pairing modal unresolved on a live session** — that's the teardown trigger, not the app lifecycle itself (backgrounded-app GATT pauses are a separate, gentler effect: `Service discovery timed out` on attaches, recoverable on retry).

## 6. Ops recipes (copy-adapt)

**Single-session agent+beacon holder** (`bt-beacon-s1.sh`; runs as user service, `Restart=always`):

```bash
#!/bin/bash
export XDG_RUNTIME_DIR=/run/user/1000
FIFO=/tmp/bt-beacon-in; rm -f $FIFO; mkfifo $FIFO
sleep infinity > $FIFO &
timeout 3600 bluetoothctl < $FIFO > /tmp/bt-beacon.log 2>&1 &
BT=$!; sleep 3
printf "agent NoInputNoOutput\ndefault-agent\nadvertise on\npairable on\ndiscoverable on\n" > $FIFO
# if advertisement registration fails, power-cycle the controller once and re-assert:
if grep -q "Failed to register" /tmp/bt-beacon.log; then
  sudo btmgmt -i hci0 power off; sleep 2; sudo btmgmt -i hci0 power on; sleep 3
  printf "advertise on\npairable on\ndiscoverable on\n" > $FIFO
fi
while kill -0 $BT 2>/dev/null; do sleep 25
  printf "advertise on\npairable on\ndiscoverable on\n" > $FIFO; done
```

*Why it looks like this:* the `sleep infinity > FIFO` keeps bluetoothctl's stdin open (a bare `bluetoothctl agent …` exits when stdin EOFs — the classic "my agent service isn't staying up" trap); the FIFO also lets you inject commands live (`printf "advertise on\n" > $FIFO`); the 25 s heartbeat re-asserts adapter flags because bluetoothd resets `Discoverable`/`Pairable` on restarts; the power-cycle clears *stuck advertisement registration state* that survives daemon restarts.

**Flag persistence (so flags survive bluetoothd restarts):**

- `/etc/bluetooth/main.conf` under `[General]`: `DiscoverableTimeout = 0` and `PairableTimeout = 0`
- Adapter state file `/var/lib/bluetooth/<adapter-mac>/settings` (system stop bluetooth first): ensure `Discoverable=true`, `Pairable=true`, both `Timeout=0`

**Identity reset** (fresh fingerprint; phone sees a brand-new peer):
```bash
systemctl --user stop witchat-relay
rm ~/.local/share/witchat/relay/identity.json   # recreated on next start
systemctl --user start witchat-relay
```

**Diagnostics crib sheet:**

| Log/mark | Meaning |
|---|---|
| `attach failed… connect …: Service discovery timed out` | phone answered connect but app-layer GATT suspended (backgrounded app) — retry when app foregrounded |
| `BLE link went silent; dropping it` | watchdog dropped a quiet link — check whether an unresolved pairing offer was pending (see §5) |
| `Failed to register advertisement` + btmon `0x0d` | peripheral advertisement dead at controller — relay continues central-only |
| `Pairing Failed (0x05) Reason: …(0x0c)` + `Disconnect Reason: Authentication Failure` | unresolved numeric-compare offer tore down the link — resolve/complete pairing (§5) |
| `noise: session established` / health `secured=1` | the real thing — session plane is healthy regardless of pairing plane |
| `link bound to peer … peer=<fp>` | app-layer attachment confirmed |

**Capture during any mystery:** `sudo btmon -w /tmp/x.cap` (replay with `btmon -r`). The SMP and MGMT lines decide agent/capability and teardown questions that logs alone can't.

## 7. What's upstream-open (pointers)

- Peripheral registration `0x0d` on CM5-class silicon (parameters accepted, data rejected) — [issue #2].
- Zero 2 W session-establishment failure with transport alive — [issue #2].
- Offline DM delivery/relay spooling — [issue #1].

*If you run a relay against phones on other hardware (dongles, other SBCs), the boards matrix in §2 is the part most worth extending — same tests, same fingerprints.*
EOF
echo DRAFT-WRITTEN