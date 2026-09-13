"use strict";

// The window is a thin client over `is-core`: every mutation goes to Rust,
// which persists it and hands back the whole state. Nothing is kept here that
// is not also on disk, so a crash cannot lose a layout.

const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);

let state = null;
/** world -> screen transform, recomputed on every render */
let transform = { scale: 1, ox: 0, oy: 0 };
let drag = null;
let probe = null;
let pendingRemoval = null;

// --------------------------------------------------------------- plumbing

// A script error used to leave the window looking fine while half of it had
// quietly stopped working. Surface it instead.
window.addEventListener("error", (event) => {
  notice(`Interface error: ${event.message} (${event.filename}:${event.lineno})`, true);
});
window.addEventListener("unhandledrejection", (event) => {
  notice(`Interface error: ${event.reason}`, true);
});

function notice(message, isError = false) {
  const node = el("notice");
  node.textContent = message;
  node.classList.toggle("error", isError);
}

async function call(command, args) {
  try {
    const result = await invoke(command, args);
    notice("");
    return result;
  } catch (error) {
    notice(String(error), true);
    throw error;
  }
}

async function apply(command, args) {
  state = await call(command, args);
  render();
}

// ------------------------------------------------------------------ render

function render() {
  if (!state.workspace) return;

  const ws = state.workspace;
  const me = ws.machines.find((m) => m.is_local);
  // The heading is this computer, not an abstraction above it. Whatever the
  // stored document is called is not something anyone has to think about.
  const name = el("workspace-name");
  const label = me ? me.name : "This computer";
  if (name.textContent !== label) name.textContent = label;

  const others = ws.machines.length - 1;
  const connected = ws.machines.filter((m) => !m.is_local && m.online).length;
  el("revision-chip").textContent =
    others === 0
      ? "on its own"
      : `${others} other computer${others === 1 ? "" : "s"} · ${connected} connected`;
  el("machine-chip").textContent = `key ${state.fingerprint}`;
  el("config-path").textContent = state.config_dir;
  // Which machine would hold the pointer right now. Worth showing: when the
  // machine that had it drops off, this is where it lands.
  // With two mice in play this is the thing people look at first, so it says
  // whether it is live or only where the pointer would land.
  el("cursor-owner").textContent = ws.cursor_owner
    ? state.sharing
      ? `cursor on ${ws.cursor_owner}`
      : `cursor would start on ${ws.cursor_owner}`
    : "";
  el("cursor-owner").classList.toggle("live", state.sharing);
  el("build-note").textContent = state.transport_port
    ? `Listening on port ${state.transport_port} · ${
        state.sharing ? "sharing keyboard and mouse" : "sharing off"
      }`
    : "Starting the network…";

  window.renderScan?.(state);
  renderSettings(ws.settings);
  renderMachineList(ws.machines);
  // The sidebar keeps a short version; the scan panel is the full story.
  renderCandidates(state.candidates);
  renderCanvas(ws.machines);
}

/**
 * Requirement 15: a machine on the network that is not in the workspace is
 * shown as something the user may admit, never admitted on its own.
 *
 * Pairing has to happen on both computers. Each one records the other's public
 * key, and neither will open a session with a key it was not told to trust —
 * so one click here is half of the handshake, not all of it.
 */
function renderCandidates(candidates) {
  const list = el("candidate-list");
  const hint = el("discovery-hint");
  list.textContent = "";

  if (!candidates.length) {
    hint.textContent =
      "No unpaired computers seen yet. Start InputShare on another machine on the same network and it will appear here.";
    return;
  }
  // A machine that has already admitted this one is a single click from
  // working. It goes first and says so plainly: the difference used to be a
  // button label changing from "Pair" to "Accept", which is not something anyone
  // notices while wondering why nothing happened.
  const ordered = [...candidates].sort(
    (a, b) => Number(b.invited_us) - Number(a.invited_us),
  );
  const waiting = ordered.filter((c) => c.invited_us);
  hint.textContent = waiting.length
    ? `${waiting.map((c) => c.display_name).join(" and ")} ${
        waiting.length === 1 ? "is" : "are"
      } waiting for you to accept.`
    : "Pair on both computers — each one has to admit the other.";
  hint.classList.toggle("calling", waiting.length > 0);

  for (const candidate of ordered) {
    const item = document.createElement("li");
    item.className = "machine";

    const status = document.createElement("span");
    status.className = "machine-status";
    status.title = "Not paired";
    const mark = document.createElement("i");
    mark.className = "unknown-mark";
    mark.textContent = "?";
    status.append(mark);

    const body = document.createElement("div");
    body.className = "machine-body";
    const label = document.createElement("div");
    label.className = "machine-name";
    label.textContent = candidate.display_name;
    const sub = document.createElement("div");
    sub.className = "machine-sub";
    sub.textContent = candidate.invited_us
      ? `wants to connect with this computer · key ${candidate.fingerprint}`
      : `key ${candidate.fingerprint}`;
    if (candidate.invited_us) item.classList.add("invited");
    body.append(label, sub);

    const actions = document.createElement("div");
    actions.className = "machine-actions";
    const pair = document.createElement("button");
    pair.className = "pill on";
    pair.textContent = candidate.invited_us ? "Accept" : "Pair";
    pair.title = candidate.invited_us
      ? `${candidate.display_name} has already admitted this computer — one click finishes it`
      : `Admit ${candidate.display_name}, checking the key shown here matches the one on that computer`;
    pair.addEventListener("click", () => apply("pair", { id: candidate.machine_id }));
    actions.append(pair);

    item.append(status, body, actions);
    list.append(item);
  }
}

function renderSettings(settings) {
  el("setting-sharing").checked = state.sharing;
  // Only overwrite while the field is not being typed in.
  const machineName = el("setting-machine-name");
  const me = state.workspace.machines.find((m) => m.is_local);
  if (document.activeElement !== machineName) {
    machineName.value = me ? me.name : "";
  }
  el("setting-offline-edge").value = settings.offline_edge;
  el("setting-clipboard").checked = settings.clipboard_sync;
  el("setting-autostart").checked = settings.autostart;
  el("setting-discovery-port").value = settings.discovery_port;
  el("setting-transport-port").value = settings.transport_port;
}

function renderMachineList(machines) {
  const list = el("machine-list");
  list.textContent = "";

  for (const machine of machines) {
    const item = document.createElement("li");
    item.className = "machine" + (machine.is_local ? " is-local" : "");

    const status = document.createElement("span");
    status.className = "machine-status";
    status.title = machine.is_local
      ? "This computer"
      : machine.online
        ? "Connected"
        : "Not reachable right now — still part of the workspace";
    const dot = document.createElement("i");
    dot.className =
      "dot " + (machine.is_local ? "local" : machine.online ? "online" : "offline");
    status.append(dot);

    const body = document.createElement("div");
    body.className = "machine-body";
    const label = document.createElement("div");
    label.className = "machine-name";
    label.textContent = machine.name;
    label.title = "Double-click to rename";
    makeRenameable(label, (next) =>
      apply("rename_machine", { id: machine.id, name: next }),
    );
    const sub = document.createElement("div");
    sub.className = "machine-sub";
    // A paired machine that has never connected has not reported its screens
    // either, and the overwhelmingly likely reason is that nobody accepted the
    // pairing on the other side. Say that, instead of leaving a bare "offline"
    // to be read as "broken".
    const waiting = !machine.is_local && !machine.online && !machine.known_displays;
    const where = machine.is_local
      ? "this computer"
      : machine.online
        ? "connected"
        : waiting
          ? "waiting to be accepted on that computer"
          : "offline";
    sub.textContent = waiting
      ? where
      : `${where} · ${machine.platform} · key ${machine.fingerprint}`;
    if (waiting) sub.classList.add("waiting");
    body.append(label, sub);

    const actions = document.createElement("div");
    actions.className = "machine-actions";
    actions.append(
      inputToggle(machine, "keyboard", "kbd"),
      inputToggle(machine, "mouse", "mouse"),
    );
    if (!machine.is_local) {
      const remove = document.createElement("button");
      remove.className = "icon-button danger";
      remove.textContent = pendingRemoval === machine.id ? "Sure?" : "✕";
      remove.title = "Unpair this computer";
      remove.addEventListener("click", async () => {
        if (pendingRemoval !== machine.id) {
          pendingRemoval = machine.id;
          render();
          setTimeout(() => {
            if (pendingRemoval === machine.id) {
              pendingRemoval = null;
              render();
            }
          }, 3000);
          return;
        }
        pendingRemoval = null;
        await apply("unpair", { id: machine.id });
      });
      actions.append(remove);
    }

    item.append(status, body, actions);
    list.append(item);
  }

}

/** Text rather than a glyph: keyboard and mouse pictographs have no reliable
    font coverage on Windows and render as tofu boxes. */
function inputToggle(machine, kind, label) {
  const button = document.createElement("button");
  const on = kind === "keyboard" ? machine.keyboard : machine.mouse;
  button.className = "pill" + (on ? " on" : "");
  button.textContent = label;
  button.title = `${on ? "Remove" : "Assign"} the ${kind} on ${machine.name}`;
  button.addEventListener("click", () =>
    apply("set_input", {
      id: machine.id,
      keyboard: kind === "keyboard" ? !machine.keyboard : machine.keyboard,
      mouse: kind === "mouse" ? !machine.mouse : machine.mouse,
    }),
  );
  return button;
}

function makeRenameable(node, commit) {
  node.addEventListener("dblclick", () => {
    node.contentEditable = "true";
    node.spellcheck = false;
    node.focus();
    document.getSelection().selectAllChildren(node);
  });
  const finish = async (save) => {
    if (node.contentEditable !== "true") return;
    node.contentEditable = "false";
    const next = node.textContent.trim();
    if (save && next) await commit(next);
    else render();
  };
  node.addEventListener("blur", () => finish(true));
  node.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault();
      finish(true);
    } else if (event.key === "Escape") {
      finish(false);
    }
  });
}

// ------------------------------------------------------------------ canvas

/// Screens a machine has not reported yet.
///
/// A record created by pairing has no displays until that machine connects and
/// publishes its own, and a machine with no displays has no size — so it used to
/// draw as a zero-pixel sliver, which reads as "nothing happened". A stand-in
/// size keeps it visible and draggable. It is never written to the document.
const ASSUMED_WIDTH = 1280;
const ASSUMED_HEIGHT = 720;

const widthOf = (machine) => machine.width || ASSUMED_WIDTH;
const heightOf = (machine) => machine.height || ASSUMED_HEIGHT;

function bounds(machines) {
  const xs = machines.flatMap((m) => [m.x, m.x + widthOf(m)]);
  const ys = machines.flatMap((m) => [m.y, m.y + heightOf(m)]);
  return {
    minX: Math.min(...xs),
    minY: Math.min(...ys),
    maxX: Math.max(...xs),
    maxY: Math.max(...ys),
  };
}

function computeTransform(machines, rect) {
  if (!machines.length) return { scale: 0.2, ox: rect.width / 2, oy: rect.height / 2 };
  const b = bounds(machines);
  const pad = 44;
  const usableW = Math.max(rect.width - pad * 2, 80);
  const usableH = Math.max(rect.height - pad * 2, 80);
  const spanX = Math.max(b.maxX - b.minX, 1);
  const spanY = Math.max(b.maxY - b.minY, 1);
  const scale = Math.min(usableW / spanX, usableH / spanY, 0.45);
  return {
    scale,
    ox: pad + (usableW - spanX * scale) / 2 - b.minX * scale,
    oy: pad + (usableH - spanY * scale) / 2 - b.minY * scale,
  };
}

const toScreenX = (x) => x * transform.scale + transform.ox;
const toScreenY = (y) => y * transform.scale + transform.oy;
const toWorldX = (sx) => (sx - transform.ox) / transform.scale;
const toWorldY = (sy) => (sy - transform.oy) / transform.scale;

function renderCanvas(machines) {
  const canvas = el("canvas");
  const world = el("world");
  transform = computeTransform(machines, canvas.getBoundingClientRect());
  world.textContent = "";

  for (const machine of machines) {
    world.append(nodeFor(machine));
  }

  if (probe) {
    const marker = document.createElement("div");
    marker.className = "probe";
    marker.style.left = `${toScreenX(probe.x)}px`;
    marker.style.top = `${toScreenY(probe.y)}px`;
    world.append(marker);
  }
}

function nodeFor(machine) {
  const node = document.createElement("div");
  node.className =
    "node" + (machine.is_local ? " local" : "") + (machine.online ? "" : " offline");
  node.dataset.id = machine.id;
  placeNode(node, machine.x, machine.y, widthOf(machine), heightOf(machine));

  const head = document.createElement("div");
  head.className = "node-head";
  const dot = document.createElement("i");
  dot.className = "dot " + (machine.is_local ? "local" : machine.online ? "online" : "offline");
  const name = document.createElement("span");
  name.className = "node-name";
  name.textContent = machine.name;
  head.append(dot, name);

  // Each physical screen, drawn inside the card. A machine with two monitors
  // should look like a machine with two monitors — and an arrangement with a
  // gap in it should be visibly a shape, not a rectangle that quietly includes
  // dead space the pointer can be pushed into.
  if (machine.screens.length > 1) {
    for (const screen of machine.screens) {
      const pane = document.createElement("div");
      pane.className = "screen" + (screen.primary ? " primary" : "");
      pane.style.left = `${(screen.x - machine.x) * transform.scale}px`;
      pane.style.top = `${(screen.y - machine.y) * transform.scale}px`;
      pane.style.width = `${screen.width * transform.scale}px`;
      pane.style.height = `${screen.height * transform.scale}px`;
      pane.title = `${screen.name} · ${screen.width}×${screen.height}`;
      node.append(pane);
    }
  }

  const meta = document.createElement("div");
  meta.className = "node-meta";
  meta.textContent = !machine.known_displays
    ? "screens unknown until it connects"
    : machine.screens.length > 1
      ? `${machine.screens.length} screens · ${machine.width}×${machine.height}`
      : `${machine.width}×${machine.height} · ${machine.x}, ${machine.y}`;
  if (!machine.known_displays) node.classList.add("unreported");

  const inputs = document.createElement("div");
  inputs.className = "node-input";
  if (machine.keyboard) inputs.append(pill("keyboard"));
  if (machine.mouse) inputs.append(pill("mouse"));

  node.append(head, meta, inputs);
  node.addEventListener("pointerdown", (event) => beginDrag(event, node, machine));
  return node;
}

function pill(text) {
  const span = document.createElement("span");
  span.className = "pill on";
  span.textContent = text;
  return span;
}

function placeNode(node, x, y, width, height) {
  node.style.left = `${toScreenX(x)}px`;
  node.style.top = `${toScreenY(y)}px`;
  node.style.width = `${width * transform.scale}px`;
  node.style.height = `${height * transform.scale}px`;
}

// -------------------------------------------------------------------- drag

function beginDrag(event, node, machine) {
  if (event.button !== 0) return;
  event.preventDefault();
  node.setPointerCapture(event.pointerId);
  node.classList.add("dragging");
  drag = {
    id: machine.id,
    node,
    machine,
    startPointer: { x: event.clientX, y: event.clientY },
    start: { x: machine.x, y: machine.y },
    current: { x: machine.x, y: machine.y },
    moved: false,
  };
}

function onDragMove(event) {
  if (!drag) return;
  const dx = (event.clientX - drag.startPointer.x) / transform.scale;
  const dy = (event.clientY - drag.startPointer.y) / transform.scale;
  if (Math.abs(dx) > 2 || Math.abs(dy) > 2) drag.moved = true;

  const others = state.workspace.machines.filter((m) => m.id !== drag.id);
  const snapped = snapPosition(
    drag.machine,
    Math.round(drag.start.x + dx),
    Math.round(drag.start.y + dy),
    others,
  );
  drag.current = snapped;
  placeNode(
    drag.node,
    snapped.x,
    snapped.y,
    widthOf(drag.machine),
    heightOf(drag.machine),
  );
  drag.node.querySelector(".node-meta").textContent =
    `${widthOf(drag.machine)}×${heightOf(drag.machine)} · ${snapped.x}, ${snapped.y}`;
}

async function endDrag(event) {
  if (!drag) return;
  const finished = drag;
  drag = null;
  finished.node.classList.remove("dragging");
  if (finished.node.hasPointerCapture?.(event.pointerId)) {
    finished.node.releasePointerCapture(event.pointerId);
  }
  if (!finished.moved) return;
  await apply("move_machine", {
    id: finished.id,
    x: finished.current.x,
    y: finished.current.y,
  });
}

/**
 * Magnetic edges. Snapping to a neighbour's edge is what makes a layout
 * actually usable for cursor crossing: a one-pixel gap would leave a seam the
 * cursor cannot cross, and an overlap would make two machines claim the same
 * space.
 */
function snapPosition(machine, x, y, others) {
  // In screen pixels, so the pull feels the same however far out the canvas is
  // zoomed. At a typical three-machine zoom this is a bit over 100 workspace
  // pixels, which is ~5% of a 1920-wide display: enough to catch a deliberate
  // drop, not enough to fight a deliberate offset.
  const threshold = 28 / transform.scale;
  const left = x;
  const right = x + widthOf(machine);
  const top = y;
  const bottom = y + heightOf(machine);

  let dx = null;
  let dy = null;
  const keep = (current, candidate) =>
    Math.abs(candidate) <= threshold &&
    (current === null || Math.abs(candidate) < Math.abs(current))
      ? candidate
      : current;

  for (const other of others) {
    const oLeft = other.x;
    const oRight = other.x + widthOf(other);
    const oTop = other.y;
    const oBottom = other.y + heightOf(other);

    // Butt up against a vertical edge, or align to one.
    dx = keep(dx, oRight - left);
    dx = keep(dx, oLeft - right);
    dx = keep(dx, oLeft - left);
    dx = keep(dx, oRight - right);

    // Same for horizontal edges.
    dy = keep(dy, oBottom - top);
    dy = keep(dy, oTop - bottom);
    dy = keep(dy, oTop - top);
    dy = keep(dy, oBottom - bottom);
  }

  return { x: x + (dx ?? 0), y: y + (dy ?? 0) };
}

window.addEventListener("pointermove", onDragMove);
window.addEventListener("pointerup", endDrag);
window.addEventListener("pointercancel", endDrag);

// ------------------------------------------------------------- route probe

el("canvas").addEventListener("click", async (event) => {
  if (event.target.closest(".node")) return;
  if (!state.workspace) return;
  const rect = el("canvas").getBoundingClientRect();
  const x = Math.round(toWorldX(event.clientX - rect.left));
  const y = Math.round(toWorldY(event.clientY - rect.top));
  probe = { x, y };

  const result = await call("resolve_route", { from: state.machine_id, x, y });
  const readout = el("route-readout");
  readout.textContent = "";
  const lead = document.createElement("span");
  lead.textContent = `cursor to ${x}, ${y} — `;
  const verdict = document.createElement("strong");
  verdict.textContent =
    result.outcome === "handoff"
      ? `crosses to ${result.machine}`
      : result.outcome === "blocked"
        ? `held on ${result.machine}`
        : `stays on ${result.machine}`;
  const why = document.createElement("span");
  why.textContent = ` (${result.explanation}, at ${result.x}, ${result.y})`;
  readout.append(lead, verdict, why);
  renderCanvas(state.workspace.machines);
});

// ----------------------------------------------------------------- chrome

// The workspace title is always editable; commit on blur, Enter blurs, Escape
// restores whatever the document says.
el("workspace-name").addEventListener("blur", async () => {
  const next = el("workspace-name").textContent.trim();
  const me = state.workspace?.machines.find((m) => m.is_local);
  if (next && me && next !== me.name) {
    await apply("rename_this_machine", { name: next });
  } else {
    render();
  }
});

/**
 * Discovery is automatic and continuous; this only skips the wait.
 *
 * It announces immediately and clears every reconnection backoff, which matters
 * when someone is standing there watching for a machine to appear — an interval
 * that the system does not notice feels like a hang to a person.
 */
el("search-network").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  button.disabled = true;
  const previous = button.textContent;
  button.textContent = "Searching…";
  try {
    await apply("rescan", {});
    notice("Announced. Machines on this network appear within a few seconds.");
  } finally {
    setTimeout(() => {
      button.disabled = false;
      button.textContent = previous;
    }, 2500);
  }
});

// Sharing is applied immediately rather than on "Apply": it is the one setting
// that changes what the keyboard does, and a switch that lies about whether it
// is on would be the worst possible place for a delay.
el("setting-sharing").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    await apply("set_sharing", { enabled });
    notice(
      enabled
        ? "Sharing on. Ctrl+Alt+F12 returns this keyboard and mouse at any time."
        : "Sharing off.",
    );
  } catch {
    event.currentTarget.checked = !enabled;
  }
});

el("toggle-settings").addEventListener("click", () => {
  const panel = el("settings-panel");
  panel.hidden = !panel.hidden;
});

el("save-settings").addEventListener("click", async () => {
  // Names first: a failure there should not be hidden behind a successful
  // settings write.
  const machineName = el("setting-machine-name").value.trim();
  const me = state.workspace.machines.find((m) => m.is_local);
  if (machineName && me && machineName !== me.name) {
    await apply("rename_this_machine", { name: machineName });
  }
  await applySettings();
  notice("Settings applied.");
});

const applySettings = () =>
  apply("update_settings", {
    settings: {
      clipboard_sync: el("setting-clipboard").checked,
      offline_edge: el("setting-offline-edge").value,
      discovery_port: Number(el("setting-discovery-port").value),
      transport_port: Number(el("setting-transport-port").value),
      autostart: el("setting-autostart").checked,
    },
  });

el("export-backup").addEventListener("click", async () => {
  const separator = state.config_dir.includes("\\") ? "\\" : "/";
  const path = `${state.config_dir}${separator}inputshare-backup.json`;
  const written = await call("export_backup", { path });
  notice(`Backup written to ${written}`);
});

new ResizeObserver(() => {
  if (state?.workspace && !drag) renderCanvas(state.workspace.machines);
}).observe(el("canvas"));

el("workspace-name").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    el("workspace-name").blur();
  } else if (event.key === "Escape") {
    event.preventDefault();
    render();
    el("workspace-name").blur();
  }
});

// Shared with scan.js, which owns the network panel.
window.InputShare = {
  adopt(next) {
    state = next;
    render();
  },
  notice,
};

// The network changes things without anyone touching the window: a peer boots,
// a peer disappears, a peer sends an edit. The agent says when, and the view
// refetches rather than polling.
window.__TAURI__.event.listen("workspace-changed", async () => {
  if (drag) return; // never yank a machine out from under the pointer
  state = await call("get_state");
  render();
});

(async () => {
  state = await call("get_state");
  render();
})();
