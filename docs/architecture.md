# Architecture

InputShare shares one keyboard and mouse across several computers. The hard part
is not moving input events; it is that the workspace has to feel permanent.
`docs/requirements.md` states it plainly: configure once, pair once, and every
boot afterwards the machines find each other and resume. Everything below is
shaped by that.

## Crate layout

| Crate | Status | Responsibility |
|---|---|---|
| `is-core` | implemented | Machine identity, the synchronized workspace document and its merge, cursor routing, local persistence. No I/O beyond the config directory. |
| `is-net` | implemented | Continuous discovery, authenticated and encrypted transport, peer state machines, reconnection backoff, document sync. |
| `is-agent` | implemented | Owns the workspace on this machine: the document on disk, the peer connections, who is reachable. Registers autostart. |
| `is-cli` | implemented | `inputshare` — inspect and edit the workspace from a terminal, and a `demo` that walks the requirements end to end. |
| `is-ui` | implemented | Tauri desktop app over the agent. Drag-and-drop topology, pairing, per-machine keyboard and mouse assignment, settings. |
| `is-input` | implemented (Windows, macOS untested) | Real monitor layout, low-level capture and replay: hooks plus raw input on Windows, `CGEventTap` plus `CGEventPost` on macOS, with key codes translated between them. Linux reports honestly that it cannot yet. |

The split exists so the agent has no dependency on the UI (requirement 4) and so
the part that must be correct — convergence and persistence — is testable without
a network or a second computer.

Today the desktop app calls `is-core` directly, because there is no agent to talk
to yet. Its command surface is deliberately coarse — whole state out, one
intent in — so that those calls become IPC to `is-agent` without the window
learning anything new.

## The three kinds of state

Keeping these apart is what makes the rest of the requirements fall out rather
than needing special cases.

**Identity** (`is-core::identity`, `identity.json`). A random UUID plus an
Ed25519 key pair, generated on first launch and never rewritten. Not derived from
IP, hostname, MAC or interface, so a laptop moving from Ethernet to Wi-Fi is
still the same machine (requirement 1). The key is what makes trust meaningful:
a reconnecting peer proves its ID rather than merely asserting it.

**Workspace** (`is-core::model`, `workspace.json`). The shared configuration:
members, display geometry, positions, input assignments, settings. Every machine
holds a full copy. It contains no addresses and no liveness, which is precisely
why a machine going offline is not a change to it (requirement 3).

**Local preferences** (`is-core::store`, `local-prefs.json`). Choices that belong
to one computer and travel nowhere — today, whether this machine is sharing its
own keyboard and mouse. Deliberately outside the workspace document: the document
is synchronized, so a switch stored in it would mean that offering this
computer's keyboard also offers everybody else's. It is still persisted, because
the critical requirement is that a reboot changes nothing the user has to redo.

**Runtime** (in-memory, plus `peer-hints.json` as a cache). Connection status,
current addresses, hostname, who owns the cursor. Rebuilt on every boot. Hints
are persisted only to skip the first discovery round; losing them costs a few
seconds, never a configuration.

## Convergence

Requirement 9 says a reconnecting machine must not be assumed to hold the latest
state, and requirement 11 says stale state must never overwrite newer state.
Wall-clock timestamps cannot deliver that — a machine that was off for a week may
have a badly skewed clock, and "newest write wins by system time" would let it
win.

So every synchronized value carries a `Stamp`: a Lamport counter plus the origin
machine ID, compared as `(lamport, origin)`. Wall-clock time is stored for
display only and never participates in ordering.

- Local edit: `lamport += 1`, stamp it, apply, persist, broadcast.
- Remote update: pull the local clock up to what was seen, then keep the value
  with the higher stamp.
- Reconnect: exchange whole documents and merge field by field.

Per-key last-writer-wins over a total order is commutative, associative and
idempotent, so all machines reach the same document regardless of the order
updates arrive in or how many times they arrive. Removals leave tombstones,
without which unpairing a machine would be undone by the next merge with a peer
that still remembered it.

Two knobs exist for the cases LWW cannot cover on its own: `prev_lamport` on each
update lets a receiver notice it has missed something and ask for a full document
instead of applying a delta it cannot place, and `schema_version` makes an older
build refuse a newer file rather than parse it and silently drop fields it does
not understand.

## Persistence

JSON in the platform config directory, overridable with
`INPUTSHARE_CONFIG_DIR` — which is also how two agents run side by side on one
box during development.

Writes are temp-file, fsync, rename, keeping the previous copy as `.bak`. Reads
fall back to `.bak` when the primary file will not parse. This is deliberate
belt-and-braces: an unparseable `workspace.json` is indistinguishable from a
first launch, so a half-written file after a power cut would otherwise re-run
onboarding and present itself to the user as exactly the reset the critical
requirement forbids. Losing the most recent write is acceptable; losing the
workspace is not.

A corrupt file with no usable backup is reported as an error, not as "no
workspace", for the same reason.

## Cursor routing

The topology is geometric: each machine occupies a rectangle in workspace
coordinates, derived from its display layout and position. It lives in the
persisted document and therefore survives machines going offline.

Routing is computed against the set of machines that are online *right now*
(`is-core::layout`). When the cursor reaches an edge:

- `Block` (default) — the nearest machine in that direction is treated as a wall
  if it is offline. The cursor stops at the boundary.
- `SkipOver` — the cursor is handed to the next online machine beyond it.

`Block` is the default because it is always recoverable and it does not
misrepresent the layout: the machine really is over there, it just cannot take
the cursor. Either way the configured route is restored automatically when the
machine reconnects, with no reconfiguration (requirement 8). `fallback_owner`
covers the other half of "never trapped": if the machine currently holding the
cursor drops off, ownership moves to an online machine, chosen identically by
every peer.

## Discovery and reconnection

Announce and listen, continuously, for the life of the process — not a scan. A
scan answers "who is up right now", which is the wrong question for machines
that boot at different times, associate to Wi-Fi slowly, or get an Ethernet
cable plugged in an hour later. Announcing on a timer means a machine that
appears at any moment is found within one interval, and no other part of the
system needs retry logic.

Announcements go to an administratively scoped multicast group and carry the
machine ID, public key, name and transport port. They are unauthenticated and
carry no trust whatsoever: anyone on the network can send one claiming anything.
All an announcement does is say where to look. Who is actually there is settled
by the handshake.

Two rules make an offline machine free, which is what requirement 7 asks for:

- A machine is only dialled if it was heard from recently. A powered-off machine
  is never dialled at all, so there are no timeouts, no retry storm and no log
  noise — the cost of a machine being off is zero, not "small".
- Of any two machines, only the one with the lower ID dials. They never race
  into two connections that then have to be torn down.

Backoff on a failed connection is exponential with a hard cap, and it resets the
moment a machine starts announcing again: a computer that has been away for a
week should not have to serve out a backoff it earned while it was gone.

Sockets are rebuilt rather than assumed permanent. A Wi-Fi reconnect, a new DHCP
lease or an interface appearing takes the socket down, and the loop re-binds
instead of going quiet until the app is restarted.

## Trust

Two questions, answered in this order. Keeping them apart is the whole security
design.

**Who are you?** The handshake. Each side sends an ephemeral X25519 public key
and a nonce, signed by its long-term Ed25519 identity key. Signing the ephemeral
key is what stops a machine in the middle from substituting its own. Both sides
derive a shared secret, run it through HKDF with both nonces, and take one
ChaCha20-Poly1305 key per direction with a counter nonce — so a tampered,
replayed or reordered frame fails to open, and the stream is protected as a
sequence rather than frame by frame.

The link is encrypted because of what it carries. Keystrokes in plaintext on a
LAN is a keylogger offered to everyone on the same Wi-Fi, and "we will add TLS
later" is how that ships.

**Do you belong here?** A separate question, answered against the public key
recorded when the machines were paired, before the peer can send a single
workspace message. Without that check, any machine on the network could prove
some identity, hand us a document, and rewrite the topology.

Pairing is mutual and explicit: each computer admits the other, by a person
comparing a key fingerprint shown on both screens. One side is never enough — but
one side is enough to *start*, because an admitted machine is named in the
admitter's announcements and can therefore show "this computer invited you".
Without that, one person clicks Pair and the other has nothing on screen to act
on, which reads as the feature being broken.

## Two configurations meeting

Every machine has a configuration from the moment it is switched on, so two
machines that pair usually have two, and two cannot merge: they have different
identifiers, which is the guarantee that stops unrelated setups blending into
each other.

This is never put to the user. Both sides run the same rule — the configuration
with more machines survives, an even split is broken by identifier — and exactly
one of them gives way, keeping its identity and a saved copy of what it gave up.
The rule is arbitrary; what matters is that both compute it identically and that
nobody is asked to arbitrate something that is an artifact of storage rather than
a decision about their desk.

An earlier version surfaced this as a banner with a choice. It was the wrong
shape: the question is unanswerable without understanding the data model, and
the honest options were identical in effect.

## The desktop app

`is-ui` is a Tauri window over the same document the CLI edits; both are clients
of `is-core`, and either can be closed without the other noticing.

The canvas is the point of it. Machines are rectangles in workspace coordinates,
dragged into place with magnetic edges — snapping matters more than it looks,
because a one-pixel gap between two machines leaves a seam the cursor cannot
cross, and an overlap makes two machines claim the same space. Every drop is
persisted before the window is told it succeeded, so closing the app can never
lose a layout.

Two pieces of honesty are built into the interface. Peers are labelled as
placeholders in the status bar, because pairing cannot be real before `is-net`
exists. And liveness is a toggle rather than a reading: clicking a machine's
status dot simulates it going offline, which is the only way to demonstrate
requirement 8 today. Clicking empty canvas asks `is-core` where the cursor would
actually go, and prints the answer.

## Sharing the keyboard and mouse

One loop, in `is-agent::sharing`, owns a single question: which machine has the
cursor? If it is this one, local input is left completely alone — nothing is
swallowed, nothing is sent, the loop only watches where the pointer is. If it is
another machine, local input is swallowed and forwarded there instead.

Crossing is decided by `is_core::layout` against the machines that are online
now, which is what stops the pointer walking onto a computer that is switched
off.

Motion travels as deltas, never as coordinates: the sending machine has no idea
where the pointer sits inside the other machine's screens. One `CursorEnter`
carries the entry point, in the receiving machine's own coordinates, so the
pointer appears at the edge it crossed rather than wherever it was left last
time. Keys travel as scan codes rather than virtual keys, so two machines with
different layouts still produce the key that was physically pressed.

Everything injected is stamped, and the capture hook ignores anything stamped.
Without that, a machine that both captures and injects would feed its own replay
straight back into the loop.

### Why that module is written the way it is

While the pointer is on another machine, this process is swallowing every
keystroke on this one. If it stops — a panic, a deadlock, a channel nobody
drains — the computer stops responding to its own keyboard, and the user may not
be able to click the thing that would fix it.

So there are four ways out, and none of them depend on the code that asked for
suppression still working:

1. **It starts off.** Capture runs in observe-only mode; swallowing is a
   separate, explicit call, and sharing is off by default in the app.
2. **A watchdog.** The routing loop has to keep saying it is alive. Miss the
   deadline and the hook releases on its own.
3. **Ctrl+Alt+F12.** Checked inside the hook itself, so it works even when
   everything above it is wedged.
4. **Windows drops slow hooks.** A backstop, not a design.

The rule behind all of it: fail open. A dropped keystroke is an annoyance; a
machine that ignores its own keyboard is an emergency.

One thing worth knowing if you change this code: `SetCursorPos` moves the
pointer without putting an event into the input stream, so a low-level hook
never sees it. That is why placing the cursor on arrival cannot loop back into
capture — and why it cannot be used to test the capture path either.

## Autostart

Requirement 4, per platform, registered per user rather than per machine — no
elevation, it follows the person, and it starts inside a desktop session, which
is where input capture has to live:

- Windows — the per-user `Run` key.
- macOS — a `LaunchAgent` plist. The Input Monitoring and Accessibility
  permissions still have to be granted by hand on first run; nothing an
  application does can grant them to itself.
- Linux — an XDG autostart entry.

## Testing

`is-core` is covered by integration tests that mirror the requirements rather
than the implementation:

- `tests/convergence.rs` — order independence, idempotence, stale-state
  rejection, tombstones, concurrent-edit tie-breaking.
- `tests/routing.rs` — crossings, the offline guard in both modes, route
  restoration, cursor-ownership fallback.
- `tests/persistence.rs` — identity stability, reboot survival, torn-write
  recovery, schema refusal, backup and restore.

Multi-machine behaviour will be tested against several agents on one host, each
pointed at its own `INPUTSHARE_CONFIG_DIR`.
