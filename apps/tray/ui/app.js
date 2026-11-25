// DisplayAnything tray window. Talks to the Rust side through Tauri commands.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

let codeTimer = null;

function notice(text) {
  const n = $("notice");
  n.textContent = text || "";
  n.hidden = !text;
}

async function loadStatus() {
  const s = await invoke("status");
  $("name").textContent = s.name;
  $("fp").textContent = s.fingerprint;
  $("port").textContent = s.port;
  $("build").textContent = s.build;
  if (s.listen_error) notice(s.listen_error);
  try { $("autostart").checked = await invoke("get_autostart"); } catch (_) { $("autostart").disabled = true; }
}

function permToggle(peer, key, label) {
  const l = document.createElement("label");
  const cb = document.createElement("input");
  cb.type = "checkbox";
  cb.checked = peer.permissions[key];
  cb.addEventListener("change", async () => {
    const permissions = { ...peer.permissions, [key]: cb.checked };
    try { await invoke("set_permissions", { fingerprint: peer.fingerprint, permissions }); peer.permissions = permissions; }
    catch (e) { cb.checked = !cb.checked; notice(String(e)); }
  });
  l.append(cb, label);
  return l;
}

// Swap `el` for a text box to edit a name in place. Enter or clicking away
// saves, Esc cancels. `save` returns a promise; on failure the box stays open.
function editInline(el, current, placeholder, save) {
  if (el.dataset.editing) return;
  el.dataset.editing = "1";
  const input = document.createElement("input");
  input.type = "text";
  input.className = "inline-edit";
  input.value = current;
  input.placeholder = placeholder;
  input.maxLength = 63;
  el.hidden = true;
  el.after(input);
  input.focus();
  input.select();
  let done = false;
  const finish = async (commit) => {
    if (done) return;
    if (commit && input.value.trim() !== current) {
      try { await save(input.value.trim()); }
      catch (e) { notice(String(e)); input.focus(); return; }
    }
    done = true;
    notice("");
    input.remove();
    el.hidden = false;
    delete el.dataset.editing;
    if (peersStale) { peersStale = false; loadPeers().then(loadInput); }
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); finish(true); }
    if (e.key === "Escape") { e.preventDefault(); finish(false); }
  });
  input.addEventListener("blur", () => finish(true));
}

function renameSelf() {
  const h = $("name");
  editInline(h, h.textContent, "Name for this computer", async (name) => {
    const s = await invoke("rename_device", { name });
    h.textContent = s.name;
  });
}

let knownPeers = [];

async function loadInput() {
  const st = await invoke("input_status");
  $("ax-banner").hidden = st.accessibility_ok;
  const sel = $("input-peer");
  const current = st.peer_fingerprint || "";
  sel.replaceChildren(new Option("Nobody", ""));
  for (const p of knownPeers) sel.append(new Option(p.name, p.fingerprint));
  sel.value = current;
  sel.disabled = !st.supported;
  $("receiving").hidden = !st.receiving_from;
  $("receiving").textContent = st.receiving_from ? `${st.receiving_from} is using this computer's keyboard and mouse.` : "";
  const share = st.share;
  const live = !!(current && share && share.peer);
  $("input-live").hidden = !live;
  if (live) {
    $("peer-name-btn").textContent = share.peer;
    $("input-dot").className = "dot" + (share.connected ? " on" : "");
    $("input-state").textContent = !share.connected
      ? `Waiting for ${share.peer}…`
      : share.remote_active ? `Keyboard and mouse are controlling ${share.peer}.` : "Keyboard and mouse are on this computer.";
    $("use-remote").disabled = !share.connected;
    // The machine that has input gets the filled button.
    $("use-remote").className = share.remote_active ? "" : "secondary";
    $("use-local").className = share.remote_active ? "secondary" : "";
  }
  $("input-error").textContent = !st.supported
    ? "Keyboard and mouse sharing works on Windows and macOS."
    : st.error ? st.error : "";
}

// ---- Shortcuts ---------------------------------------------------------

const IS_MAC = navigator.userAgent.includes("Mac");
const NAMED_KEYS = {
  0x28: "Enter", 0x29: "Esc", 0x2a: "Backspace", 0x2b: "Tab", 0x2c: "Space", 0x2d: "-", 0x2e: "=",
  0x2f: "[", 0x30: "]", 0x31: "\\", 0x32: "#", 0x33: ";", 0x34: "'", 0x35: "`", 0x36: ",", 0x37: ".",
  0x38: "/", 0x39: "Caps Lock", 0x46: "Print Screen", 0x47: "Scroll Lock", 0x48: "Pause", 0x49: "Insert",
  0x4a: "Home", 0x4b: "Page Up", 0x4c: "Delete", 0x4d: "End", 0x4e: "Page Down", 0x4f: "→", 0x50: "←",
  0x51: "↓", 0x52: "↑", 0x53: "Num Lock", 0x54: "Num /", 0x55: "Num *", 0x56: "Num -", 0x57: "Num +",
  0x58: "Num Enter", 0x62: "Num 0", 0x63: "Num .", 0x64: "\\", 0x65: "Menu", 0x67: "Num =",
};
function keyLabel(u) {
  if (u >= 0x04 && u <= 0x1d) return String.fromCharCode(65 + u - 0x04);
  if (u >= 0x1e && u <= 0x26) return String(u - 0x1d);
  if (u === 0x27) return "0";
  if (u >= 0x3a && u <= 0x45) return `F${u - 0x39}`;
  if (u >= 0x68 && u <= 0x73) return `F${u - 0x68 + 13}`;
  if (u >= 0x59 && u <= 0x61) return `Num ${u - 0x58}`;
  switch (u) {
    case 0xe0: case 0xe4: return "Ctrl";
    case 0xe1: case 0xe5: return "Shift";
    case 0xe2: case 0xe6: return IS_MAC ? "Option" : "Alt";
    case 0xe3: case 0xe7: return IS_MAC ? "Cmd" : "Win";
  }
  return NAMED_KEYS[u] || `Key ${u.toString(16)}`;
}
const BUTTON_LABELS = { left: "Left click", right: "Right click", middle: "Middle click", back: "Mouse Back", forward: "Mouse Forward" };

function comboNode(combo) {
  const frag = document.createDocumentFragment();
  const parts = [...combo.keys.map(keyLabel), ...combo.buttons.map((b) => BUTTON_LABELS[b] || b)];
  parts.forEach((p, i) => {
    if (i) frag.append(" + ");
    const k = document.createElement("kbd");
    k.textContent = p;
    frag.append(k);
  });
  return frag;
}

let recordingAction = null;
let recordTimeout = null;
const ACTION_NAMES = { to_remote: "switching to the other computer", to_local: "coming back", panic: "always coming back", edge_lock: "pausing edge crossing" };

async function loadEdge() {
  const v = await invoke("get_edge");
  $("edge").value = v.edge || "";
  $("edge-locked").checked = v.locked;
  $("edge-locked").disabled = !v.edge;
}

async function loadHotkeys() {
  const v = await invoke("get_hotkeys");
  for (const row of document.querySelectorAll(".hk-row")) {
    row.querySelector(".hk-combo").replaceChildren(comboNode(v.hotkeys[row.dataset.action]));
  }
  $("hint-remote").replaceChildren(comboNode(v.hotkeys.to_remote));
  $("hint-local").replaceChildren(comboNode(v.hotkeys.to_local));
  $("hint-panic").replaceChildren(comboNode(v.hotkeys.panic));
  $("hk-reset").hidden = v.is_default;
}

function stopRecordingUi() {
  recordingAction = null;
  clearTimeout(recordTimeout);
  $("hk-recorder").hidden = true;
  document.querySelectorAll(".hk-row").forEach((r) => r.classList.remove("recording"));
  document.querySelectorAll(".hk-change").forEach((b) => (b.disabled = false));
}

async function startRecording(action) {
  $("hk-msg").textContent = "";
  try { await invoke("record_hotkey", { action }); }
  catch (e) { $("hk-msg").textContent = String(e); return; }
  recordingAction = action;
  document.querySelectorAll(".hk-change").forEach((b) => (b.disabled = true));
  document.querySelector(`.hk-row[data-action="${action}"]`).classList.add("recording");
  $("hk-prompt").textContent = `Press and hold the keys and/or mouse buttons for ${ACTION_NAMES[action]}, and keep holding for 2 seconds.`;
  $("hk-live").textContent = "Waiting for keys…";
  $("hk-bar").style.width = "0";
  $("hk-recorder").hidden = false;
  // Give up after a while so input isn't held hostage.
  recordTimeout = setTimeout(() => { invoke("cancel_hotkey_record"); stopRecordingUi(); $("hk-msg").textContent = "Recording timed out; nothing changed."; }, 30000);
}

function onHotkeyHeld(ev) {
  if (!recordingAction) return;
  const empty = !ev.combo.keys.length && !ev.combo.buttons.length;
  if (empty) $("hk-live").textContent = "Waiting for keys…";
  else $("hk-live").replaceChildren(comboNode(ev.combo));
  $("hk-bar").style.width = empty ? "0" : `${Math.min(100, (ev.held_ms / ev.needed_ms) * 100)}%`;
}

async function onHotkeyDone(ev) {
  stopRecordingUi();
  $("hk-msg").textContent = ev.error ? `Not saved: ${ev.error}` : "Saved.";
  await loadHotkeys();
}

let peersStale = false;

async function loadPeers() {
  // Don't redraw the list under someone typing a new name; catch up after.
  if (document.querySelector("#peers .inline-edit")) { peersStale = true; return; }
  const peers = await invoke("peers");
  knownPeers = peers;
  const ul = $("peers");
  ul.replaceChildren();
  $("empty").hidden = peers.length > 0;
  for (const p of peers) {
    const li = document.createElement("li");
    const head = document.createElement("div");
    head.className = "peer-head";
    const title = document.createElement("div");
    const dot = document.createElement("span");
    dot.className = "dot" + (p.online ? " on" : "");
    const name = document.createElement("strong");
    name.textContent = p.name;
    const meta = document.createElement("div");
    meta.className = "muted";
    meta.textContent = `${p.online ? "Connected" : "Offline"} · ${p.last_addr || "no address yet"} · ${p.short}`;
    title.append(dot, name, meta);
    const forget = document.createElement("button");
    forget.className = "link";
    forget.textContent = "Forget";
    forget.addEventListener("click", async () => {
      if (forget.dataset.armed) { await invoke("unpair", { fingerprint: p.fingerprint }); loadPeers(); }
      else { forget.dataset.armed = "1"; forget.textContent = "Click again to forget"; }
    });
    const rename = document.createElement("button");
    rename.className = "link";
    rename.textContent = "Rename";
    rename.title = p.custom_name
      ? "Clear the name to go back to the name this device gives itself"
      : "Choose what this device is called on this computer";
    rename.addEventListener("click", () =>
      editInline(name, p.name, "Leave empty to use its own name", async (newName) => {
        await invoke("rename_peer", { fingerprint: p.fingerprint, name: newName });
        peersStale = true; // redraw once the text box closes
      }));
    const actions = document.createElement("div");
    actions.className = "actions";
    actions.append(rename, forget);
    head.append(title, actions);

    const perms = document.createElement("div");
    perms.className = "perms";
    const auto = document.createElement("label");
    const autoCb = document.createElement("input");
    autoCb.type = "checkbox";
    autoCb.checked = p.auto_connect;
    autoCb.addEventListener("change", () =>
      invoke("set_auto_connect", { fingerprint: p.fingerprint, on: autoCb.checked }).catch((e) => notice(String(e))));
    auto.append(autoCb, "Connect automatically");
    perms.append(
      auto,
      permToggle(p, "input_takeover", "May control this computer with its keyboard and mouse"),
      permToggle(p, "display_takeover", "May take over my monitor"),
      permToggle(p, "remote_control", "May control this desktop"),
    );
    li.append(head, perms);
    ul.append(li);
  }
}

function showCode(ev) {
  $("code-peer").textContent = `${ev.peer_name} (${ev.peer_ip})`;
  $("code").textContent = ev.code.slice(0, 3) + " " + ev.code.slice(3);
  $("code-banner").hidden = false;
  let left = ev.expires_in_secs;
  $("code-ttl").textContent = left;
  clearInterval(codeTimer);
  codeTimer = setInterval(() => {
    left -= 1;
    $("code-ttl").textContent = Math.max(left, 0);
    if (left <= 0) hideCode();
  }, 1000);
}

function hideCode() {
  clearInterval(codeTimer);
  $("code-banner").hidden = true;
}

$("pair-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const addr = $("pair-addr").value.trim();
  $("pair-btn").disabled = true;
  $("pair-msg").textContent = `Contacting ${addr}…`;
  try {
    const name = await invoke("pair", { addr });
    $("pair-msg").textContent = `Paired with ${name}. It will reconnect automatically from now on.`;
    $("pair-addr").value = "";
  } catch (err) {
    $("pair-msg").textContent = `Pairing failed: ${err}`;
  } finally {
    $("pair-btn").disabled = false;
    $("code-form").hidden = true;
    loadPeers();
  }
});

$("code-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  $("pair-msg").textContent = "Checking code…";
  try { await invoke("submit_code", { code: $("code-input").value }); } catch (err) { $("pair-msg").textContent = String(err); }
  $("code-input").value = "";
  $("code-form").hidden = true;
});

$("cancel-btn").addEventListener("click", () => {
  invoke("cancel_pair");
  $("code-form").hidden = true;
});

$("discover-btn").addEventListener("click", async () => {
  const btn = $("discover-btn");
  btn.disabled = true;
  btn.textContent = "Searching…";
  const ul = $("found");
  ul.replaceChildren();
  try {
    const found = await invoke("discover");
    if (!found.length) {
      const li = document.createElement("li");
      li.className = "muted";
      li.textContent = "Nothing found. You can still pair by typing the IP address.";
      ul.append(li);
    }
    for (const [name, addr] of found) {
      const li = document.createElement("li");
      const label = document.createElement("span");
      label.textContent = `${name} · ${addr}`;
      const use = document.createElement("button");
      use.className = "secondary";
      use.textContent = "Use";
      use.addEventListener("click", () => { $("pair-addr").value = addr; ul.replaceChildren(); });
      li.append(label, use);
      ul.append(li);
    }
  } catch (err) {
    notice(`Discovery failed: ${err}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Find on network";
  }
});

$("input-peer").addEventListener("change", async (e) => {
  try { await invoke("set_input_peer", { fingerprint: e.target.value || null }); }
  catch (err) { $("input-error").textContent = String(err); }
  loadInput();
});
$("use-remote").addEventListener("click", () => invoke("switch_input", { remote: true }).catch((e) => notice(String(e))));
$("use-local").addEventListener("click", () => invoke("switch_input", { remote: false }).catch((e) => notice(String(e))));
for (const row of document.querySelectorAll(".hk-row")) {
  row.querySelector(".hk-change").addEventListener("click", () => startRecording(row.dataset.action));
}
$("hk-cancel").addEventListener("click", () => { invoke("cancel_hotkey_record"); stopRecordingUi(); $("hk-msg").textContent = "Cancelled; nothing changed."; });
$("hk-reset").addEventListener("click", async () => { await invoke("reset_hotkeys"); $("hk-msg").textContent = "Shortcuts reset to the defaults."; loadHotkeys(); });
$("edge").addEventListener("change", async (e) => { await invoke("set_edge", { edge: e.target.value || null }); loadEdge(); });
$("edge-locked").addEventListener("change", (e) => invoke("set_edge_locked", { locked: e.target.checked }));
$("logging").addEventListener("change", (e) => invoke("set_logging", { on: e.target.checked }));
$("log-btn2").addEventListener("click", () => invoke("show_log").catch((e) => notice(String(e))));
$("log-btn").addEventListener("click", () => invoke("show_log").catch((e) => notice(String(e))));
$("rename-btn").addEventListener("click", renameSelf);
$("name").addEventListener("click", renameSelf);
$("ax-btn").addEventListener("click", async () => { await invoke("request_accessibility"); setTimeout(loadInput, 1500); });

$("autostart").addEventListener("change", async (e) => {
  try { await invoke("set_autostart", { on: e.target.checked }); }
  catch (err) { e.target.checked = !e.target.checked; notice(String(err)); }
});

listen("da-need-code", () => {
  $("pair-msg").textContent = "The other device is showing a code.";
  $("code-form").hidden = false;
  $("code-input").focus();
});

listen("da-event", ({ payload: ev }) => {
  switch (ev.kind) {
    case "pair_code": showCode(ev); break;
    case "paired": hideCode(); notice(""); loadPeers(); break;
    case "pair_failed": hideCode(); break;
    case "peers_changed": loadPeers().then(loadInput); break;
    case "notice": notice(ev.text); break;
    case "input_changed": loadInput(); break;
    case "hotkey_held": onHotkeyHeld(ev); break;
    case "edge_changed": loadEdge(); break;
    case "hotkey_done": onHotkeyDone(ev); break;
  }
});

loadStatus();
loadPeers().then(loadInput);
loadHotkeys();
loadEdge();
invoke("get_logging").then((on) => ($("logging").checked = on));
// Accessibility can be granted in System Settings while we are open.
setInterval(loadInput, 5000);
