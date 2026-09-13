<h1 align="center">InputShare</h1>

<p align="center">
  One keyboard and one mouse across several computers —<br>
  as a workspace that stays configured.
</p>

<p align="center">
  <img alt="license" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue">
  <img alt="rust" src="https://img.shields.io/badge/rust-1.77%2B-orange">
  <img alt="platforms" src="https://img.shields.io/badge/input-Windows%20%7C%20macOS-lightgrey">
  <img alt="tests" src="https://img.shields.io/badge/tests-44%20passing-brightgreen">
</p>

---

Push the pointer off the edge of one screen and it arrives on the next computer,
with the keyboard following it. Pair the machines once. Then turn them all off,
move house, come back a week later on a different network with different IP
addresses — they find each other, prove who they are, reconnect and resume.

**Nothing has to be set up twice.** That is the whole point, and it is the one
requirement every other decision here was made to serve.

```text
        ┌─────────────────────┐      ┌──────────────────┐
        │                     │      │                  │
        │       Studio PC     │ ───▶ │    Laptop        │
        │       2560×1440     │  ▲   │    1920×1200     │
        │                     │  │   │                  │
        └─────────────────────┘  │   └──────────────────┘
                                 │
                    the pointer crosses here,
                    and the keyboard goes with it
```

## What it does

| | |
|---|---|
| **Finds the other machines by itself** | UDP multicast on every interface, with a broadcast fallback and manual entry for networks that block both |
| **Proves who they are** | X25519 ECDH signed with each machine's permanent Ed25519 key, then ChaCha20-Poly1305. A machine talks to nobody it was not explicitly told to trust |
| **Survives everything** | Reboots, new IP addresses, a week powered off, being unplugged mid-write. Identity and configuration are on disk and crash-safe |
| **Agrees without a server** | Every machine holds the whole configuration; concurrent edits merge deterministically by Lamport stamp, so two machines that were both edited offline converge on the same answer |
| **Knows your real monitors** | Enumerated per machine, not assumed. A laptop with an external screen occupies the shape it actually has, including the gaps in an L |
| **Hands the pointer over cleanly** | One machine owns the cursor at a time, motion is read from the device rather than from the clamped pointer, and the crossing point is where you actually left |
| **Starts with the computer** | Per user, not per machine: no elevation, and it starts inside a desktop session, which is where input capture has to live |

## Safety

Sharing a keyboard means a program that can *swallow* your keyboard. That is one
bug away from a computer which no longer responds to its own input, so:

- **It starts off.** Capture begins in observe-only mode. Swallowing is a
  separate, deliberate switch.
- **A watchdog.** The hook has to keep hearing from the rest of the program.
  Two seconds of silence and it releases on its own.
- **An escape hatch.** `Ctrl+Alt+F12` — `Control+Option+F12` on a Mac — is
  checked inside the hook itself, so it works even when everything above it is
  wedged.

The rule behind all three: fail open. A dropped keystroke is an annoyance; a
machine that ignores its own keyboard is an emergency.

## Status

Honest version:

| | |
|---|---|
| Discovery, pairing, encryption, reconnection, merge, persistence, autostart | **done, tested** |
| Input capture and replay on **Windows** | **done**, running across two real machines |
| Input capture and replay on **macOS** | **done**, crossing works; being shaken out |
| Input capture and replay on **Linux** | not implemented; it says so rather than failing quietly |

The macOS backend is a `CGEventTap` for capture, `CGEventPost` for replay,
`CGGetActiveDisplayList` for the screens, and a key-code table that translates in
both directions so a PC keyboard produces the key that was physically pressed.

One macOS detail worth writing down, because it is not obvious and it looks like
a haunting: swallowing an event in a tap does **not** stop the cursor. The HID
system has already moved it by the time a tap is called, so an app that only
returns null keeps sliding its own arrow around while the pointer is supposed to
be on another computer. The mouse has to be detached from the cursor with
`CGAssociateMouseAndMouseCursorPosition(false)` for as long as this machine is
not the one the pointer is on — and every path that stops suppressing, watchdog
and emergency release included, has to attach it again.

## Install

**Windows** — build the installer:

```bash
cargo install tauri-cli
cargo tauri build --config crates/is-ui/tauri.conf.json
```

**macOS** — on the Mac, with Rust and `xcode-select --install`:

```bash
tools/build-macos.sh
```

`--universal` builds one app for both Apple Silicon and Intel. There is no
cross-compiling from Windows: a Mac application has to be linked against Apple's
SDK and signed with Apple's tools. The script ad-hoc signs the app, which is what
makes macOS remember the permissions it was granted instead of asking again after
every rebuild.

macOS then needs three permissions, and no application can grant them to itself:

> System Settings → Privacy & Security → **Accessibility** → InputShare
>
> System Settings → Privacy & Security → **Input Monitoring** → InputShare
>
> System Settings → Privacy & Security → **Local Network** → InputShare

The last one is the one that wastes an afternoon. Since macOS 15 an application
is refused the local network in total silence until it is granted: no error, no
peers, an empty list that looks exactly like a network with nothing on it.

## Using it

Open the app on both computers. There is no setup step — a computer on its own is
already a complete configuration of one machine.

1. Press **Pair** next to the other machine. It is told it was invited and shows
   **Accept**. Both sides record the other's public key, and the fingerprint is
   shown on both screens so you can check they match.
2. Drag the machines around the canvas until their edges touch the way your desk
   does.
3. Turn on **Share this keyboard and mouse** on the computer whose keyboard you
   want to use.

Push the pointer past an edge and it crosses.

The sharing switch is remembered per computer, so a machine that was sharing when
it was shut down is sharing again when it comes back. It is deliberately *not*
part of the synchronized configuration: it is this computer's keyboard being
offered, not a property of the workspace.

If the two machines had separate configurations before they met, one gives way
automatically. Both sides pick the same survivor from the same numbers, so nobody
is asked to arbitrate an artifact of how the data is stored.

## How it is built

A Rust workspace, with a Tauri window over a library that knows nothing about the
window — so the agent never depends on the UI being open.

```text
crates/is-core     identity, workspace document, merge, routing, persistence
crates/is-net      discovery, encrypted transport, reconnection, sync
crates/is-input    monitors, keyboard and mouse capture, replay
crates/is-agent    owns the workspace on this machine; sharing; autostart
crates/is-cli      inputshare      — terminal client
crates/is-ui       InputShare      — desktop app
tools/             icon generation, the macOS build
docs/              requirements and architecture
```

Two decisions worth knowing before reading the code.

**Time is a Lamport stamp, never a clock.** A machine that was off for a week may
come back with a badly skewed clock, and "newest write wins by system time" would
let it overwrite everything. Every synchronized value carries `(lamport, origin)`
and is compared on that alone.

**Motion is read from the device, not from the cursor.** Both Windows and macOS
stop the pointer at the edge of the screen, so a reading taken from the pointer
goes to zero at the exact moment somebody is pushing towards the next computer.
Raw Input and `kCGMouseEventDeltaX` report what the hardware did instead. That one
took a while to find, and there is a probe in the repo that demonstrates it:

```bash
cargo run -p is-input --example edge_probe
```

The requirements are in [docs/requirements.md](docs/requirements.md); the design
and the reasoning in [docs/architecture.md](docs/architecture.md).

## Developing

```bash
cargo test --workspace        # 44 tests, no network or second computer needed
cargo run -p is-ui            # the desktop app
cargo run -p is-cli -- demo   # walks the requirements end to end in a terminal
```

Type-check the macOS code from any machine — this catches most of what would
otherwise only show up on the Mac:

```bash
rustup target add aarch64-apple-darwin
cargo clippy -p is-input -p is-agent -p is-net --target aarch64-apple-darwin --all-targets
```

`INPUTSHARE_CONFIG_DIR` overrides the config directory, which is how you run two
instances on one machine. Icons are generated rather than committed as opaque
blobs: `node tools/make-icons.mjs`.

Probes, all safe to run at any time — none of them ever swallows input:

```bash
cargo run -p is-input --example displays        # what this machine's monitors really are
cargo run -p is-input --example capture_probe   # four seconds of observe-only capture
cargo run -p is-net   --example netcheck        # which cards, who is heard, who answers
```

Run `netcheck` on both machines at once when discovery is not working. Give it
the other machine's address and it also tests the TCP port pairing uses, because
"cannot see it" and "sees it but cannot pair" are different faults:

```bash
cargo run -p is-net --example netcheck -- 192.168.1.42
```

## Licence

MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
