// Интерфейс клиента. Весь звук и сеть — в Rust (src/main.rs), здесь только
// отрисовка и вызов команд через invoke().

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

// Настройки храним в localStorage окна приложения.
const store = {
  get(key, fallback) {
    try {
      const raw = localStorage.getItem(key);
      return raw === null ? fallback : JSON.parse(raw);
    } catch {
      return fallback;
    }
  },
  set(key, value) {
    try {
      localStorage.setItem(key, JSON.stringify(value));
    } catch {
      // Не страшно: просто не запомним.
    }
  },
};

const call = {
  // Сведения о своём сервере, если комнату создали мы.
  hosting: null,
  me: null, // мы сами: { id, name, admin, muted, deafened, serverMuted }
  peers: new Map(), // id -> такие же сведения о собеседнике
  statusTimer: null,
};

// ---------------------------------------------------------------------------
// Иконки (SVG 24×24, цвет — currentColor)
// ---------------------------------------------------------------------------

const ICON_PATHS = {
  mic: "M12 14a3 3 0 0 0 3-3V5a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2Z",
  headphones:
    "M12 3a9 9 0 0 0-9 9v7a2 2 0 0 0 2 2h3v-8H5v-1a7 7 0 0 1 14 0v1h-3v8h3a2 2 0 0 0 2-2v-7a9 9 0 0 0-9-9Z",
  crown: "M5 16 3 5l5.5 5L12 4l3.5 6L21 5l-2 11H5Zm14 3a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1v-1h14v1Z",
  more: "M12 8a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm0 2a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm0 6a2 2 0 1 0 0 4 2 2 0 0 0 0-4Z",
};

// Иконка; `crossed` — перечёркнутая (выключено).
function icon(name, crossed = false) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "icon");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", ICON_PATHS[name]);
  svg.append(path);
  if (crossed) {
    const line = document.createElementNS(ns, "line");
    for (const [k, v] of Object.entries({ x1: 3, y1: 3, x2: 21, y2: 21 })) line.setAttribute(k, v);
    line.setAttribute("class", "icon-cross");
    svg.append(line);
  }
  return svg;
}

// ---------------------------------------------------------------------------
// Общее
// ---------------------------------------------------------------------------

let toastTimer = null;
function toast(text) {
  const el = $("toast");
  el.textContent = text;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 5000);
}

function showView(name) {
  $("connect-view").hidden = name !== "connect";
  $("room-view").hidden = name !== "room";
}

// Стабильный цвет аватара по имени.
function avatarColor(name) {
  let hash = 0;
  for (const ch of name) hash = (hash * 31 + ch.codePointAt(0)) >>> 0;
  const palette = ["#5865f2", "#eb459e", "#3ba55d", "#faa61a", "#ed4245", "#9b59b6", "#1abc9c", "#e67e22"];
  return palette[hash % palette.length];
}

function initial(name) {
  return (name.trim()[0] || "?").toUpperCase();
}

// ---------------------------------------------------------------------------
// Экран подключения
// ---------------------------------------------------------------------------

async function loadDevices() {
  try {
    const devices = await invoke("list_devices");
    fillSelect($("input"), devices.inputs, devices.defaultInput, store.get("input", ""));
    fillSelect($("output"), devices.outputs, devices.defaultOutput, store.get("output", ""));
  } catch (e) {
    toast(`Не удалось получить список устройств: ${e}`);
  }
}

function fillSelect(select, names, systemDefault, saved) {
  select.innerHTML = "";
  const def = document.createElement("option");
  def.value = "";
  def.textContent = systemDefault ? `По умолчанию (${systemDefault})` : "По умолчанию";
  select.append(def);
  for (const name of names) {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    select.append(opt);
  }
  select.value = names.includes(saved) ? saved : "";
}

// ---------------------------------------------------------------------------
// Поиск серверов в локальной сети
// ---------------------------------------------------------------------------

const LAN_REFRESH_MS = 5000;
let lanTimer = null;
let lanSearching = false;

async function refreshLan() {
  if (lanSearching || $("connect-view").hidden) return;
  lanSearching = true;
  try {
    renderLan(await invoke("discover_servers"));
  } catch (e) {
    $("lan-status").textContent = `Поиск не удался: ${e}`;
  } finally {
    lanSearching = false;
  }
}

function renderLan(servers) {
  const list = $("lan-list");
  list.innerHTML = "";
  for (const server of servers) {
    const li = document.createElement("li");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "lan-item";
    button.classList.toggle("selected", $("server").value.trim() === server.address);
    button.disabled = !server.compatible;

    const name = document.createElement("span");
    name.className = "lan-name";
    name.textContent = server.name;
    const meta = document.createElement("span");
    meta.className = "lan-meta";
    meta.textContent = server.compatible
      ? `${server.host} · ${server.users} чел.`
      : `${server.host} · другая версия программы`;
    button.append(name, meta);

    button.addEventListener("click", () => {
      $("server").value = server.address;
      for (const other of list.querySelectorAll(".lan-item")) other.classList.remove("selected");
      button.classList.add("selected");
      if (!$("name").value.trim()) $("name").focus();
    });
    li.append(button);
    list.append(li);
  }
  $("lan-status").textContent = servers.length
    ? ""
    : "Не найдено. Через интернет (Tailscale) адрес вводится вручную.";
  $("lan-status").hidden = servers.length > 0;
}

function startLanSearch() {
  clearInterval(lanTimer);
  refreshLan();
  lanTimer = setInterval(refreshLan, LAN_REFRESH_MS);
}

function stopLanSearch() {
  clearInterval(lanTimer);
  lanTimer = null;
}

$("lan-refresh").addEventListener("click", () => {
  $("lan-status").hidden = false;
  $("lan-status").textContent = "Ищу серверы…";
  refreshLan();
});

// Читает и запоминает поля формы.
function formValues() {
  const values = {
    server: $("server").value.trim(),
    name: $("name").value.trim(),
    input: $("input").value,
    output: $("output").value,
  };
  for (const [key, value] of Object.entries(values)) store.set(key, value);
  return values;
}

$("connect-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const { server, name, input, output } = formValues();

  const button = $("connect-btn");
  const error = $("connect-error");
  button.disabled = true;
  button.textContent = "Подключение…";
  error.hidden = true;
  $("cert-warning").hidden = true;
  try {
    const joined = await invoke("connect", { args: { server, name, input, output } });
    enterRoom(server, name, joined);
  } catch (e) {
    if (e?.kind === "certificateChanged") {
      showCertWarning(e);
    } else {
      error.textContent = errorText(e);
      error.hidden = false;
    }
  } finally {
    button.disabled = false;
    button.textContent = "Подключиться";
  }
});

// Ошибки команд приходят строкой, а ошибки подключения — объектом.
function errorText(e) {
  return e?.message ?? String(e);
}

// Сертификат сервера не совпал с запомненным: решает пользователь.
let pendingCertificate = null;

function showCertWarning(e) {
  pendingCertificate = e;
  $("cert-address").textContent = e.address;
  $("cert-known").textContent = e.knownCode;
  $("cert-presented").textContent = e.presentedCode;
  $("cert-warning").hidden = false;
}

$("cert-cancel").addEventListener("click", () => {
  pendingCertificate = null;
  $("cert-warning").hidden = true;
});

$("cert-trust").addEventListener("click", async () => {
  if (!pendingCertificate) return;
  const { address, presented } = pendingCertificate;
  pendingCertificate = null;
  $("cert-warning").hidden = true;
  try {
    await invoke("trust_server", { address, fingerprint: presented });
    $("connect-form").requestSubmit();
  } catch (e) {
    toast(`Не удалось запомнить сертификат: ${errorText(e)}`);
  }
});

// «Создать комнату»: запускаем сервер внутри программы и подключаемся к нему сами.
$("host-btn").addEventListener("click", async () => {
  if (!$("name").value.trim()) {
    $("name").reportValidity();
    return;
  }
  const { name, input, output } = formValues();
  const button = $("host-btn");
  const error = $("connect-error");
  button.disabled = true;
  button.textContent = "Запуск сервера…";
  error.hidden = true;
  let hosted = null;
  try {
    hosted = await invoke("start_server", { name: `${name} — комната` });
    const joined = await invoke("connect", {
      args: { server: hosted.localAddress, name, input, output },
    });
    call.hosting = hosted.info;
    enterRoom("Ваш сервер", name, joined);
  } catch (e) {
    if (hosted) await invoke("stop_server").catch(() => {});
    error.textContent = errorText(e);
    error.hidden = false;
  } finally {
    button.disabled = false;
    button.textContent = "Создать комнату на этом компьютере";
  }
});

// ---------------------------------------------------------------------------
// Комната
// ---------------------------------------------------------------------------

function enterRoom(server, name, joined) {
  call.me = joined.me;
  call.peers = new Map(joined.peers.map((p) => [p.id, p]));
  $("server-label").textContent = server;
  $("server-code").textContent = `код ${joined.serverCode}`;
  renderHostCard();
  disarmLeave();
  renderPeers();
  for (const [id, peer] of call.peers) applySavedVolume(id, peer.name);
  showView("room");
  stopLanSearch();
  clearInterval(call.statusTimer);
  call.statusTimer = setInterval(refreshStatus, 100);
}

function leaveRoom(reason) {
  call.hosting = null;
  clearInterval(call.statusTimer);
  call.statusTimer = null;
  call.peers.clear();
  showView("connect");
  startLanSearch();
  if (reason) toast(reason);
}

// Хосту закрытие комнаты отключает всех, поэтому просим нажать дважды.
let leaveArmTimer = null;

function disarmLeave() {
  clearTimeout(leaveArmTimer);
  const button = $("leave-btn");
  button.classList.remove("armed");
  button.textContent = call.hosting ? "Закрыть комнату" : "Отключиться";
}

$("leave-btn").addEventListener("click", async () => {
  const button = $("leave-btn");
  if (call.hosting && !button.classList.contains("armed")) {
    button.classList.add("armed");
    button.textContent = "Все отключатся. Ещё раз?";
    leaveArmTimer = setTimeout(disarmLeave, 4000);
    return;
  }
  const wasHosting = Boolean(call.hosting);
  try {
    await invoke("disconnect");
    if (wasHosting) await invoke("stop_server");
  } finally {
    leaveRoom();
  }
});

function renderHostCard() {
  const card = $("host-card");
  card.hidden = !call.hosting;
  if (!call.hosting) return;
  card.open = store.get("hostCardOpen", false);
  $("host-code").textContent = call.hosting.code;
  const list = $("host-addresses");
  list.innerHTML = "";
  for (const { interface: iface, address } of call.hosting.addresses) {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.className = "host-iface";
    name.textContent = iface;
    name.title = iface;
    const addr = document.createElement("span");
    addr.className = "host-address";
    addr.textContent = address;
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "link";
    copy.textContent = "копировать";
    copy.addEventListener("click", () => copyText(address));
    li.append(name, addr, copy);
    list.append(li);
  }
  if (call.hosting.addresses.length === 0) {
    const li = document.createElement("li");
    li.className = "subtle small-text";
    li.textContent = "Компьютер не подключён к сети.";
    list.append(li);
  }
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Запасной путь для движков, где Clipboard API недоступен.
    const area = document.createElement("textarea");
    area.value = text;
    document.body.append(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  toast(`Скопировано: ${text}`);
}

function renderPeers() {
  const list = $("peers");
  list.innerHTML = "";
  list.append(peerItem(call.me, true));
  for (const peer of [...call.peers.values()].sort((a, b) => a.id - b.id)) {
    list.append(peerItem(peer, false));
  }
  if (call.peers.size === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = "Пока никого нет. Позовите друзей!";
    list.append(empty);
  }
}

function peerItem(peer, isMe) {
  const li = document.createElement("li");
  li.className = "peer";
  li.dataset.id = peer.id;
  if (isMe) li.classList.add("me");

  const avatar = document.createElement("div");
  avatar.className = "avatar";
  avatar.style.background = avatarColor(peer.name);
  avatar.textContent = initial(peer.name);

  const main = document.createElement("div");
  main.className = "peer-main";
  const title = document.createElement("div");
  title.className = "peer-name";
  if (peer.admin) {
    const crown = icon("crown");
    crown.classList.add("crown");
    crown.append(Object.assign(document.createElementNS("http://www.w3.org/2000/svg", "title"), {
      textContent: "Хост комнаты",
    }));
    title.append(crown);
  }
  title.append(document.createTextNode(peer.name));
  if (isMe) {
    const you = document.createElement("span");
    you.className = "you";
    you.textContent = " (вы)";
    title.append(you);
  }
  main.append(title);

  // Что с человеком: заглушён хостом, сам выключил микрофон или звук.
  const notes = [];
  if (peer.serverMuted) notes.push(isMe ? "хост заглушил вас для всех" : "заглушён хостом");
  if (notes.length) {
    const badge = document.createElement("div");
    badge.className = "badge off";
    badge.textContent = notes.join(", ");
    main.append(badge);
  }
  if (!isMe) main.append(volumeControl(peer.id, peer.name));

  const side = document.createElement("div");
  side.className = "peer-side";
  if (isMe) {
    side.append(
      toggleButton("my-mic", icon("mic", peer.muted), peer.muted, "Выключить/включить микрофон", () =>
        setMuted(!call.me.muted),
      ),
      toggleButton("my-sound", icon("headphones", peer.deafened), peer.deafened, "Выключить/включить звук", () =>
        setDeafened(!call.me.deafened),
      ),
    );
  } else {
    // Статус собеседника — просто значки.
    if (peer.muted) side.append(statusIcon(icon("mic", true), "Выключил микрофон"));
    if (peer.deafened) side.append(statusIcon(icon("headphones", true), "Выключил звук"));
    // Права хоста — в меню, чтобы не путать с значками статуса.
    if (call.me.admin) side.append(hostMenu(peer));
  }

  li.append(avatar, main, side);
  return li;
}

function toggleButton(id, iconEl, off, title, onClick) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "icon-btn";
  if (id) button.id = id;
  button.classList.toggle("off", off);
  button.title = title;
  button.append(iconEl);
  button.addEventListener("click", onClick);
  return button;
}

function statusIcon(iconEl, title) {
  const span = document.createElement("span");
  span.className = "status-icon";
  span.title = title;
  span.append(iconEl);
  return span;
}

// Меню хоста для участника: заглушить для всех, выгнать.
function hostMenu(peer) {
  const wrap = document.createElement("div");
  wrap.className = "menu-wrap";
  const button = toggleButton(null, icon("more"), false, "Действия хоста", (event) => {
    event.stopPropagation();
    const open = !menu.hidden;
    closeMenus();
    menu.hidden = open;
  });
  // Без всплывающей подсказки: она закрывала бы открытое меню.
  button.removeAttribute("title");
  button.setAttribute("aria-label", "Действия хоста");
  const menu = document.createElement("div");
  menu.className = "menu";
  menu.hidden = true;

  const mute = document.createElement("button");
  mute.type = "button";
  mute.textContent = peer.serverMuted ? "Вернуть голос" : "Заглушить для всех";
  mute.addEventListener("click", () => {
    closeMenus();
    invoke("set_server_mute", { id: peer.id, muted: !peer.serverMuted });
  });

  // Выгнать — нужно подтвердить вторым нажатием.
  const kick = document.createElement("button");
  kick.type = "button";
  kick.className = "danger-item";
  kick.textContent = "Выгнать из комнаты";
  kick.addEventListener("click", (event) => {
    event.stopPropagation();
    if (!kick.classList.contains("armed")) {
      kick.classList.add("armed");
      kick.textContent = "Точно выгнать?";
      return;
    }
    closeMenus();
    invoke("kick", { id: peer.id });
  });

  menu.append(mute, kick);
  wrap.append(button, menu);
  return wrap;
}

function closeMenus() {
  for (const menu of document.querySelectorAll(".menu")) menu.hidden = true;
}

document.addEventListener("click", closeMenus);

function setMuted(muted) {
  call.me.muted = muted;
  renderPeers();
  invoke("set_muted", { muted });
}

function setDeafened(deafened) {
  call.me.deafened = deafened;
  renderPeers();
  invoke("set_deafened", { deafened });
}

function volumeControl(id, name) {
  const row = document.createElement("div");
  row.className = "peer-volume";
  const slider = document.createElement("input");
  slider.type = "range";
  slider.min = 0;
  slider.max = 200;
  slider.step = 5;
  slider.value = savedVolumes()[name] ?? 100;
  slider.title = "Громкость собеседника";
  const value = document.createElement("output");
  value.textContent = `${slider.value}%`;
  slider.addEventListener("input", () => {
    value.textContent = `${slider.value}%`;
    const volumes = savedVolumes();
    volumes[name] = Number(slider.value);
    store.set("volumes", volumes);
    invoke("set_volume", { id, volume: slider.value / 100 }).catch(() => {});
  });
  row.append(slider, value);
  return row;
}

// Громкость запоминаем по имени: id у человека меняется при каждом подключении.
function savedVolumes() {
  return store.get("volumes", {});
}

function applySavedVolume(id, name) {
  const percent = savedVolumes()[name];
  if (percent !== undefined && percent !== 100) {
    invoke("set_volume", { id, volume: percent / 100 }).catch(() => {});
  }
}

listen("peer-joined", ({ payload }) => {
  call.peers.set(payload.id, payload);
  applySavedVolume(payload.id, payload.name);
  renderPeers();
});

listen("peer-left", ({ payload }) => {
  call.peers.delete(payload.id);
  renderPeers();
});

listen("peer-updated", ({ payload }) => {
  if (!call.me) return;
  if (payload.id === call.me.id) {
    if (payload.serverMuted !== call.me.serverMuted) {
      toast(payload.serverMuted ? "Хост заглушил вас: остальные вас не слышат" : "Хост вернул вам голос");
    }
    // Свои mute/deafen знаем точнее сервера: он мог ещё не получить последнее нажатие.
    call.me = { ...payload, muted: call.me.muted, deafened: call.me.deafened };
  } else {
    call.peers.set(payload.id, payload);
  }
  renderPeers();
});

listen("call-ended", async ({ payload }) => {
  await invoke("disconnect").catch(() => {});
  if (call.hosting) await invoke("stop_server").catch(() => {});
  leaveRoom(`Звонок завершён: ${payload.reason}`);
});

// ---------------------------------------------------------------------------
// Индикаторы
// ---------------------------------------------------------------------------

const METER_MIN_DB = -70;
const dbToPercent = (db) => Math.max(0, Math.min(100, ((db - METER_MIN_DB) / -METER_MIN_DB) * 100));

async function refreshStatus() {
  let status;
  try {
    status = await invoke("status");
  } catch {
    return;
  }
  if (!status.connected) return;

  const bar = $("level-bar");
  bar.style.width = `${dbToPercent(status.levelDb)}%`;
  bar.classList.toggle("active", status.transmitting);

  const speaking = new Set(status.speaking);
  for (const li of document.querySelectorAll(".peer")) {
    const id = Number(li.dataset.id);
    const active = li.classList.contains("me") ? status.transmitting : speaking.has(id);
    li.classList.toggle("speaking", active);
  }


  const glitches = $("glitches");
  if (status.micGlitches || status.outputGlitches) {
    glitches.textContent = `Сбои звука: микрофон ${status.micGlitches}, вывод ${status.outputGlitches}`;
    glitches.hidden = false;
  } else {
    glitches.hidden = true;
  }
}


// ---------------------------------------------------------------------------
// Управление передачей
// ---------------------------------------------------------------------------

$("host-card").addEventListener("toggle", () => store.set("hostCardOpen", $("host-card").open));

$("mode").addEventListener("change", async () => {
  const mode = $("mode").value;
  store.set("mode", mode);
  updateModeRows();
  try {
    await invoke("set_mode", { mode });
  } catch (e) {
    toast(String(e));
  }
});

function updateModeRows() {
  const mode = $("mode").value;
  $("vad-row").hidden = mode !== "voice";
  $("ptt-row").hidden = mode !== "ptt";
  $("ptt-hint").hidden = mode !== "ptt";
  $("threshold-mark").hidden = mode !== "voice";
}

$("denoise").addEventListener("change", () => {
  const enabled = $("denoise").checked;
  store.set("denoise", enabled);
  invoke("set_noise_suppression", { enabled });
});

$("vad").addEventListener("input", () => {
  const db = Number($("vad").value);
  store.set("vad", db);
  showVad(db);
  invoke("set_vad_threshold", { db });
});

function showVad(db) {
  $("vad-value").textContent = `${db} dB`;
  $("threshold-mark").style.left = `${dbToPercent(db)}%`;
}

// Назначение клавиши рации: нажимаем кнопку, затем нужную клавишу.
const MODIFIER_CODES = new Set([
  "ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight",
]);
let capturingKey = false;

$("ptt-key").addEventListener("click", () => {
  capturingKey = true;
  $("ptt-key").textContent = "Нажмите клавишу… (Esc — отмена)";
  $("ptt-key").classList.add("listening");
});

window.addEventListener("keydown", async (event) => {
  if (!capturingKey || MODIFIER_CODES.has(event.code)) return;
  event.preventDefault();
  capturingKey = false;
  $("ptt-key").classList.remove("listening");
  if (event.code === "Escape") {
    showPttKey(store.get("pttKey", null));
    return;
  }
  const parts = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  parts.push(event.code);
  const key = parts.join("+");
  try {
    await invoke("set_ptt_key", { key });
    store.set("pttKey", key);
    showPttKey(key);
  } catch (e) {
    toast(String(e));
    showPttKey(store.get("pttKey", null));
  }
});

// "Ctrl+KeyV" -> "Ctrl+V", "Digit1" -> "1"
function showPttKey(key) {
  $("ptt-key").textContent = key ? key.replace(/Key([A-Z])/, "$1").replace(/Digit(\d)/, "$1") : "не назначена";
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

async function init() {
  $("server").value = store.get("server", "");
  $("name").value = store.get("name", "");

  const mode = store.get("mode", "voice");
  const vad = store.get("vad", -45);
  const pttKey = store.get("pttKey", null);
  const denoise = store.get("denoise", true);
  $("mode").value = mode;
  $("denoise").checked = denoise;
  $("vad").value = vad;
  showVad(vad);
  showPttKey(pttKey);
  updateModeRows();

  startLanSearch();
  await loadDevices();
  try {
    await invoke("set_noise_suppression", { enabled: denoise });
    await invoke("set_vad_threshold", { db: vad });
    await invoke("set_ptt_key", { key: pttKey });
    await invoke("set_mode", { mode });
  } catch (e) {
    toast(String(e));
  }
}

init();
