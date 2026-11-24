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

let knownPeers = [];

async function loadInput() {
  const st = await invoke("input_status");
  const isMac = navigator.userAgent.includes("Mac");
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
  const mod = isMac ? "Ctrl+Option" : "Ctrl+Alt";
  document.querySelectorAll("#k-mod, .k-mod2").forEach((k) => (k.textContent = mod));
  $("input-error").textContent = !st.supported
    ? "Keyboard and mouse sharing works on Windows and macOS."
    : st.error ? st.error : "";
}

async function loadPeers() {
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
    head.append(title, forget);

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
  }
});

loadStatus();
loadPeers().then(loadInput);
// Accessibility can be granted in System Settings while we are open.
setInterval(loadInput, 5000);
