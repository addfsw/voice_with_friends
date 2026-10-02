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
  myId: null,
  myName: "",
  peers: new Map(), // id -> имя
  statusTimer: null,
};

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

$("connect-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const server = $("server").value.trim();
  const name = $("name").value.trim();
  const input = $("input").value;
  const output = $("output").value;
  store.set("server", server);
  store.set("name", name);
  store.set("input", input);
  store.set("output", output);

  const button = $("connect-btn");
  const error = $("connect-error");
  button.disabled = true;
  button.textContent = "Подключение…";
  error.hidden = true;
  try {
    const joined = await invoke("connect", { args: { server, name, input, output } });
    enterRoom(server, name, joined);
  } catch (e) {
    error.textContent = String(e);
    error.hidden = false;
  } finally {
    button.disabled = false;
    button.textContent = "Подключиться";
  }
});

// ---------------------------------------------------------------------------
// Комната
// ---------------------------------------------------------------------------

function enterRoom(server, name, joined) {
  call.myId = joined.myId;
  call.myName = name;
  call.peers = new Map(joined.peers.map((p) => [p.id, p.name]));
  $("server-label").textContent = server;
  renderPeers();
  for (const [id, peerName] of call.peers) applySavedVolume(id, peerName);
  showView("room");
  clearInterval(call.statusTimer);
  call.statusTimer = setInterval(refreshStatus, 100);
}

function leaveRoom(reason) {
  clearInterval(call.statusTimer);
  call.statusTimer = null;
  call.peers.clear();
  showView("connect");
  if (reason) toast(reason);
}

$("leave-btn").addEventListener("click", async () => {
  try {
    await invoke("disconnect");
  } finally {
    leaveRoom();
  }
});

function renderPeers() {
  const list = $("peers");
  list.innerHTML = "";
  list.append(peerItem(call.myId, call.myName, true));
  for (const [id, name] of [...call.peers].sort((a, b) => a[0] - b[0])) {
    list.append(peerItem(id, name, false));
  }
  if (call.peers.size === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = "Пока никого нет. Позовите друзей!";
    list.append(empty);
  }
}

function peerItem(id, name, isMe) {
  const li = document.createElement("li");
  li.className = "peer";
  li.dataset.id = id;

  const avatar = document.createElement("div");
  avatar.className = "avatar";
  avatar.style.background = avatarColor(name);
  avatar.textContent = initial(name);

  const main = document.createElement("div");
  main.className = "peer-main";
  const title = document.createElement("div");
  title.className = "peer-name";
  title.textContent = name;
  main.append(title);

  if (isMe) {
    li.classList.add("me");
    const you = document.createElement("span");
    you.className = "you";
    you.textContent = " (вы)";
    title.append(you);
    const badge = document.createElement("div");
    badge.className = "badge";
    badge.id = "my-badge";
    main.append(badge);
  } else {
    main.append(volumeControl(id, name));
  }

  li.append(avatar, main);
  return li;
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
  call.peers.set(payload.id, payload.name);
  applySavedVolume(payload.id, payload.name);
  renderPeers();
});

listen("peer-left", ({ payload }) => {
  call.peers.delete(payload.id);
  renderPeers();
});

listen("call-ended", async ({ payload }) => {
  await invoke("disconnect").catch(() => {});
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

  const badge = $("my-badge");
  if (badge) {
    const parts = [];
    if (status.muted) parts.push("микрофон выключен");
    if (status.deafened) parts.push("звук выключен");
    badge.textContent = parts.join(", ");
    badge.classList.toggle("off", parts.length > 0);
  }
  setToggle($("mute-btn"), !status.muted, MIC_LABELS);
  setToggle($("deafen-btn"), !status.deafened, SOUND_LABELS);

  const glitches = $("glitches");
  if (status.micGlitches || status.outputGlitches) {
    glitches.textContent = `Сбои звука: микрофон ${status.micGlitches}, вывод ${status.outputGlitches}`;
    glitches.hidden = false;
  } else {
    glitches.hidden = true;
  }
}

const MIC_LABELS = ["Микрофон", "Микрофон выкл."];
const SOUND_LABELS = ["Звук", "Звук выкл."];

function setToggle(button, on, [onText, offText]) {
  button.textContent = on ? onText : offText;
  button.classList.toggle("off", !on);
}

// ---------------------------------------------------------------------------
// Управление передачей
// ---------------------------------------------------------------------------

$("mute-btn").addEventListener("click", () => {
  const muted = !$("mute-btn").classList.contains("off");
  setToggle($("mute-btn"), !muted, MIC_LABELS);
  invoke("set_muted", { muted });
});

$("deafen-btn").addEventListener("click", () => {
  const deafened = !$("deafen-btn").classList.contains("off");
  setToggle($("deafen-btn"), !deafened, SOUND_LABELS);
  invoke("set_deafened", { deafened });
});

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
  $("mode").value = mode;
  $("vad").value = vad;
  showVad(vad);
  showPttKey(pttKey);
  updateModeRows();

  await loadDevices();
  try {
    await invoke("set_vad_threshold", { db: vad });
    await invoke("set_ptt_key", { key: pttKey });
    await invoke("set_mode", { mode });
  } catch (e) {
    toast(String(e));
  }
}

init();
