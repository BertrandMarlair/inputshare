"use strict";

// The network view.
//
// Discovery is automatic, so this panel exists for the moment it does not work:
// somebody is standing there wondering why the other computer has not appeared.
// The question it has to answer is "what is it even looking at" — which is why
// the interfaces are listed with their addresses. The commonest cause of silence
// is that the only card in play is a virtual one, or that the two machines are
// on different networks, and both are obvious the second you can see the
// addresses side by side.
//
// The radar is not decoration either. Discovery has no progress to report — it
// announces and waits — so the animation is what distinguishes "looking" from
// "frozen" while nothing is being found.

(() => {
  const { invoke } = window.__TAURI__.core;
  const el = (id) => document.getElementById(id);

  let open = false;

  function setOpen(next) {
    open = next;
    el("scan").hidden = !open;
    if (open) {
      invoke("rescan").then((state) => window.InputShare?.adopt(state));
      el("manual-address").focus();
    }
  }

  el("search-network").addEventListener("click", () => setOpen(true));
  el("scan-close").addEventListener("click", () => setOpen(false));
  el("scan").addEventListener("click", (event) => {
    if (event.target === el("scan")) setOpen(false);
  });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && open) setOpen(false);
  });

  el("scan-rescan").addEventListener("click", async (event) => {
    const button = event.currentTarget;
    button.disabled = true;
    el("radar").classList.add("pinging");
    try {
      const state = await invoke("rescan");
      window.InputShare?.adopt(state);
    } finally {
      setTimeout(() => {
        button.disabled = false;
        el("radar").classList.remove("pinging");
      }, 1200);
    }
  });

  el("manual-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const field = el("manual-address");
    const address = field.value.trim();
    if (!address) return;
    try {
      const state = await invoke("add_manual_peer", { address });
      window.InputShare?.adopt(state);
      field.value = "";
      setStatus(`Asking ${address} directly…`);
    } catch (error) {
      setStatus(String(error), true);
    }
  });

  function setStatus(text, isError = false) {
    const node = el("manual-status");
    node.textContent = text;
    node.classList.toggle("error", isError);
  }

  /// A stable angle per machine, so a blip does not jump around between renders.
  function angleFor(id) {
    let hash = 0;
    for (let i = 0; i < id.length; i++) {
      hash = (hash * 31 + id.charCodeAt(i)) >>> 0;
    }
    return (hash % 360) * (Math.PI / 180);
  }

  function renderBlips(candidates, machines) {
    const layer = el("blips");
    layer.textContent = "";

    // Paired machines sit on the inner ring, unpaired ones further out: the
    // distance means "how far from being usable", not anything about the
    // network.
    const points = [
      ...machines
        .filter((m) => !m.is_local && m.online)
        .map((m) => ({ id: m.id, label: m.name, ring: 0.36, kind: "online" })),
      ...candidates.map((c) => ({
        id: c.machine_id,
        label: c.display_name,
        ring: 0.72,
        kind: "candidate",
      })),
    ];

    for (const point of points) {
      const angle = angleFor(point.id);
      const blip = document.createElement("div");
      blip.className = `blip ${point.kind}`;
      blip.style.left = `${50 + Math.cos(angle) * point.ring * 50}%`;
      blip.style.top = `${50 + Math.sin(angle) * point.ring * 50}%`;
      blip.title = point.label;
      const dot = document.createElement("i");
      const label = document.createElement("span");
      label.textContent = point.label;
      blip.append(dot, label);
      layer.append(blip);
    }
  }

  function renderInterfaces(interfaces) {
    const list = el("scan-interfaces");
    list.textContent = "";
    const usable = interfaces.filter((i) => !i.self_assigned);

    for (const item of interfaces) {
      const row = document.createElement("li");
      row.className = "iface" + (item.self_assigned ? " weak" : "");
      const name = document.createElement("span");
      name.className = "iface-name";
      name.textContent = item.name;
      const address = document.createElement("span");
      address.className = "iface-address";
      address.textContent = item.address;
      row.append(name, address);
      if (item.self_assigned) {
        const note = document.createElement("span");
        note.className = "iface-note";
        note.textContent = "no address from a router";
        row.append(note);
      }
      list.append(row);
    }

    const hint = el("interfaces-hint");
    if (!interfaces.length) {
      hint.textContent = "No usable network card. Nothing can be found.";
    } else if (!usable.length) {
      hint.textContent =
        "Every card here has a self-assigned address, which means none of them reached a router.";
    } else {
      hint.textContent =
        "The other computer has to be on one of these networks. Compare the numbers with what it shows.";
    }
  }

  function renderFound(candidates, machines) {
    const list = el("scan-found");
    list.textContent = "";

    const online = machines.filter((m) => !m.is_local && m.online);
    for (const machine of online) {
      list.append(
        row({
          mark: "●",
          markClass: "online",
          title: machine.name,
          detail: "paired and connected",
        }),
      );
    }

    // Invited machines first: they are one click from working, and burying
    // them under the rest is how a person concludes that nothing happened.
    const ordered = [...candidates].sort(
      (a, b) => Number(b.invited_us) - Number(a.invited_us),
    );

    for (const candidate of ordered) {
      const seen =
        candidate.seconds_since_seen < 5
          ? "just now"
          : `${candidate.seconds_since_seen}s ago`;
      const detail = candidate.invited_us
        ? `invited this computer · ${candidate.addr} · key ${candidate.fingerprint}`
        : `${candidate.addr} · seen ${seen} · key ${candidate.fingerprint}`;
      const item = row({
        mark: "?",
        markClass: candidate.invited_us ? "invited" : "unknown",
        title: candidate.display_name,
        detail,
      });
      if (candidate.invited_us) item.classList.add("invited");
      const pair = document.createElement("button");
      pair.className = "pill on";
      pair.textContent = candidate.invited_us
        ? "Accept"
        : candidate.has_workspace
          ? "Join"
          : "Pair";
      pair.addEventListener("click", async () => {
        const state = await invoke("pair", { id: candidate.machine_id });
        window.InputShare?.adopt(state);
      });
      item.querySelector(".machine-actions").append(pair);
      list.append(item);
    }

    el("scan-empty").textContent =
      online.length + candidates.length === 0
        ? "Nothing yet. A computer running InputShare on the same network shows up within a few seconds."
        : "";
  }

  function row({ mark, markClass, title, detail }) {
    const item = document.createElement("li");
    item.className = "machine";

    const status = document.createElement("span");
    status.className = "machine-status";
    const glyph = document.createElement("i");
    const isMark = markClass === "unknown" || markClass === "invited";
    glyph.className = isMark ? `unknown-mark ${markClass}` : `dot ${markClass}`;
    if (isMark) glyph.textContent = markClass === "invited" ? "!" : mark;
    status.append(glyph);

    const body = document.createElement("div");
    body.className = "machine-body";
    const name = document.createElement("div");
    name.className = "machine-name";
    name.textContent = title;
    const sub = document.createElement("div");
    sub.className = "machine-sub";
    sub.textContent = detail;
    body.append(name, sub);

    const actions = document.createElement("div");
    actions.className = "machine-actions";

    item.append(status, body, actions);
    return item;
  }

  function renderManual(addresses) {
    const list = el("manual-list");
    list.textContent = "";
    for (const address of addresses) {
      const item = document.createElement("li");
      item.className = "manual-entry";
      const label = document.createElement("span");
      label.textContent = address;
      const remove = document.createElement("button");
      remove.className = "icon-button";
      remove.textContent = "✕";
      remove.title = `Stop contacting ${address}`;
      remove.addEventListener("click", async () => {
        const state = await invoke("forget_manual_peer", { address });
        window.InputShare?.adopt(state);
      });
      item.append(label, remove);
      list.append(item);
    }
  }

  window.renderScan = (state) => {
    const machines = state.workspace ? state.workspace.machines : [];
    el("scan-sub").textContent = state.transport_port
      ? `Announcing on ${state.interfaces.length} network card${
          state.interfaces.length === 1 ? "" : "s"
        }, port ${state.discovery_port}`
      : "Starting the network…";
    renderBlips(state.candidates, machines);
    renderInterfaces(state.interfaces);
    renderFound(state.candidates, machines);
    renderManual(state.manual_peers);
  };
})();
