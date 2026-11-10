// Интерфейс клиента. Весь звук и сеть — в Rust (src/main.rs), здесь только
// отрисовка и вызов команд через invoke().

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

// На Android программа — приложение для телефона: MainActivity.kt даёт
// `window.PoigrulkinyAndroid` (фоновый звонок, цвет системных панелей).
const isAndroid = document.documentElement.classList.contains("android");
const androidBridge = window.PoigrulkinyAndroid ?? null;

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
  // Сведения о своём сервере, если комнату открыли мы.
  hosting: null,
  hostRoom: null, // настройки открытой своей комнаты: { id, name, lan, internet }
  me: null, // мы сами: { id, name, admin, muted, deafened, serverMuted }
  peers: new Map(), // id -> такие же сведения о собеседнике
  statusTimer: null,
  qualityTimer: null,
  quality: new Map(), // id -> { level: "good" | "fair" | "bad", text } — качество связи
  chat: [], // сообщения чата: { id, from, name, text, timeMs }, старые — первыми
  chatPending: [], // пришли, пока интерфейс ещё не вошёл в комнату
  title: "", // название комнаты для уведомлений Android ("" — неизвестно)
  unread: 0,
  sharing: false, // мы показываем экран
  viewers: 0, // сколько человек смотрит наш экран
  camera: false, // наша камера включена
  cameraId: "", // какая (id из camera_sources)
  cameraViewers: 0, // сколько человек смотрит нашу камеру (во всех слоях)
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
  gear:
    "M19.14 12.94c.04-.31.06-.62.06-.94s-.02-.63-.06-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96a7.03 7.03 0 0 0-1.62-.94l-.36-2.54A.48.48 0 0 0 13.93 2h-3.84a.48.48 0 0 0-.48.41l-.36 2.54c-.59.24-1.13.56-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.73 8.47a.48.48 0 0 0 .12.61l2.03 1.58c-.04.31-.07.63-.07.94s.02.63.06.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32a.48.48 0 0 0-.12-.61l-2.01-1.58ZM12 15.6A3.6 3.6 0 1 1 12 8.4a3.6 3.6 0 0 1 0 7.2Z",
  chevron: "M7.41 8.59 12 13.17l4.59-4.58L18 10l-6 6-6-6 1.41-1.41Z",
  close:
    "M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12 19 6.41Z",
  check: "M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41L9 16.17Z",
  logout:
    "M10.09 15.59 11.5 17l5-5-5-5-1.41 1.41L12.67 11H3v2h9.67l-2.58 2.59ZM19 3H5a2 2 0 0 0-2 2v4h2V5h14v14H5v-4H3v4a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V5a2 2 0 0 0-2-2Z",
  broadcast:
    "M12 11a2 2 0 1 0 0 4 2 2 0 0 0 0-4Zm6 2a6 6 0 1 0-9 5.19l1-1.74a4 4 0 1 1 4 0l1 1.74A6 6 0 0 0 18 13ZM12 3a10 10 0 0 0-5 18.66l1-1.73a8 8 0 1 1 8 0l1 1.73A10 10 0 0 0 12 3Z",
  copy: "M16 1H4a2 2 0 0 0-2 2v14h2V3h12V1Zm3 4H8a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2Zm0 16H8V7h11v14Z",
  send: "M2.01 21 23 12 2.01 3 2 10l15 2-15 2z",
  tune:
    "M3 17v2h6v-2H3ZM3 5v2h10V5H3Zm10 16v-2h8v-2h-8v-2h-2v6h2ZM7 9v2H3v2h4v2h2V9H7Zm14 4v-2H11v2h10Zm-6-4h2V7h4V5h-4V3h-2v6Z",
  screen:
    "M21 3H3a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h5v2h8v-2h5a2 2 0 0 0 2-2V5a2 2 0 0 0-2-2Zm0 14H3V5h18v12Z",
  camera: "M17 10.5V7a1 1 0 0 0-1-1H4a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-3.5l4 4v-11l-4 4Z",
  "camera-flip":
    "M16 7h-1l-1-1h-4L9 7H8a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2Zm-4 7a2 2 0 1 1 0-4 2 2 0 0 1 0 4ZM8.57.51l4.48 4.48V2.04A10 10 0 0 1 22 11h2C23.34 4.02 16.65-1.11 8.57.51Zm2.38 21.45A10 10 0 0 1 2 13H0c.66 6.97 7.35 12.1 15.43 10.48l-4.48-4.48v2.95Z",
  lock: "M18 8h-1V6A5 5 0 0 0 7 6v2H6a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V10a2 2 0 0 0-2-2Zm-6 9a2 2 0 1 1 0-4 2 2 0 0 1 0 4Zm3.1-9H8.9V6a3.1 3.1 0 0 1 6.2 0v2Z",
  plus: "M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2Z",
  "lock-open":
    "M12 17a2 2 0 1 0 0-4 2 2 0 0 0 0 4Zm6-9h-1V6A5 5 0 0 0 7 6h1.9a3.1 3.1 0 0 1 6.2 0v2H6a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V10a2 2 0 0 0-2-2Zm0 12H6V10h12v10Z",
  eye: "M12 4.5C7 4.5 2.73 7.61 1 12c1.73 4.39 6 7.5 11 7.5s9.27-3.11 11-7.5c-1.73-4.39-6-7.5-11-7.5ZM12 17a5 5 0 1 1 0-10 5 5 0 0 1 0 10Zm0-8a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z",
  "view-list": "M3 13h2v-2H3v2Zm0 4h2v-2H3v2Zm0-8h2V7H3v2Zm4 4h14v-2H7v2Zm0 4h14v-2H7v2ZM7 7v2h14V7H7Z",
  "view-grid":
    "M3 3v8h8V3H3Zm6 6H5V5h4v4Zm-6 4v8h8v-8H3Zm6 6H5v-4h4v4Zm4-16v8h8V3h-8Zm6 6h-4V5h4v4Zm-6 4v8h8v-8h-8Zm6 6h-4v-4h4v4Z",
  "person-add":
    "M15 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8Zm-9-2V7H4v3H1v2h3v3h2v-3h3v-2H6Zm9 4c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4Z",
  wifi: "M1 9l2 2c4.97-4.97 13.03-4.97 18 0l2-2C16.93 2.93 7.08 2.93 1 9Zm8 8 3 3 3-3a4.24 4.24 0 0 0-6 0Zm-4-4 2 2a7.07 7.07 0 0 1 10 0l2-2C15.14 9.14 8.87 9.14 5 13Z",
  history:
    "M13 3a9 9 0 0 0-9 9H1l3.89 3.89.07.14L9 12H6a7 7 0 1 1 2.05 4.95l-1.42 1.42A9 9 0 1 0 13 3Zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12Z",
  link: "M3.9 12a3.1 3.1 0 0 1 3.1-3.1h4V7H7a5 5 0 0 0 0 10h4v-1.9H7A3.1 3.1 0 0 1 3.9 12ZM8 13h8v-2H8v2Zm9-6h-4v1.9h4a3.1 3.1 0 0 1 0 6.2h-4V17h4a5 5 0 0 0 0-10Z",
  "chevron-right": "M8.59 16.59 13.17 12 8.59 7.41 10 6l6 6-6 6-1.41-1.41Z",
  "chevron-left": "M15.41 16.59 10.83 12l4.58-4.59L14 6l-6 6 6 6 1.41-1.41Z",
  palette:
    "M12 3a9 9 0 0 0 0 18c.83 0 1.5-.67 1.5-1.5 0-.39-.15-.74-.39-1.01-.23-.26-.38-.61-.38-.99 0-.83.67-1.5 1.5-1.5H16c2.76 0 5-2.24 5-5 0-4.42-4.03-8-9-8Zm-5.5 9a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Zm3-4a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Zm5 0a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Zm3 4a1.5 1.5 0 1 1 0-3 1.5 1.5 0 0 1 0 3Z",
  rotate:
    "M16.48 2.52c3.27 1.55 5.61 4.72 5.97 8.48h1.5C23.44 4.84 18.29 0 12 0l-.66.03 3.81 3.81 1.33-1.32Zm-6.25-.77a1.49 1.49 0 0 0-2.12 0L1.75 8.11a1.49 1.49 0 0 0 0 2.12l12.02 12.02c.59.59 1.54.59 2.12 0l6.36-6.36c.59-.59.59-1.54 0-2.12L10.23 1.75Zm4.6 19.44L2.81 9.17l6.36-6.36 12.02 12.02-6.36 6.36ZM7.52 21.48C4.25 19.94 1.91 16.76 1.55 13H.05C.56 19.16 5.71 24 12 24l.66-.03-3.81-3.81-1.33 1.32Z",
  "volume-off":
    "M16.5 12A4.5 4.5 0 0 0 14 7.97v2.21l2.45 2.45c.03-.2.05-.41.05-.63Zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51A8.8 8.8 0 0 0 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71ZM4.27 3 3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06a8.99 8.99 0 0 0 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3ZM12 4 9.91 6.09 12 8.18V4Z",
  signal: "M3 16h3v5H3v-5Zm5.5-4h3v9h-3v-9ZM14 8h3v13h-3V8Zm5.5-4h3v17h-3V4Z",
  "signal-fair": "M3 16h3v5H3v-5Zm5.5-4h3v9h-3v-9ZM14 8h3v13h-3V8Z",
  "signal-bad": "M3 16h3v5H3v-5Z",
  window: "M19 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2Zm0 14H5V8h14v10Z",
  help:
    "M11 18h2v-2h-2v2Zm1-16a10 10 0 1 0 0 20 10 10 0 0 0 0-20Zm0 18a8 8 0 1 1 0-16 8 8 0 0 1 0 16Zm0-14a4 4 0 0 0-4 4h2a2 2 0 1 1 4 0c0 2-3 1.75-3 5h2c0-2.25 3-2.5 3-5a4 4 0 0 0-4-4Z",
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

// Значки в разметке: <span data-icon="gear"></span>.
for (const el of document.querySelectorAll("[data-icon]")) el.prepend(icon(el.dataset.icon));

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
  const palette = ["#5b5bd6", "#d6409f", "#2f9e6e", "#e08a00", "#e5484d", "#8b5cf6", "#0b8fd8", "#e5622c"];
  return palette[hash % palette.length];
}

// Аватарки участников: id → data:-адрес картинки. Своя — отдельно, она
// есть и до подключения.
const avatars = new Map();
let myAvatar = null;

function avatarOf(id) {
  return call.me && id === call.me.id ? myAvatar : avatars.get(id) ?? null;
}

// Аватар: картинка, если есть, иначе первая буква имени на цветном круге.
function paintAvatar(el, name, url) {
  el.classList.toggle("has-image", Boolean(url));
  if (url) {
    el.textContent = "";
    el.style.background = `center / cover no-repeat url("${url}")`;
  } else {
    el.textContent = initial(name);
    el.style.background = avatarColor(name);
  }
}

function initial(name) {
  return (name.trim()[0] || "?").toUpperCase();
}

// ---------------------------------------------------------------------------
// Экран подключения
// ---------------------------------------------------------------------------

// Списки устройств: { inputs, outputs, defaultInput, defaultOutput }.
let devices = null;

async function loadDevices() {
  try {
    devices = await invoke("list_devices");
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
  select.value = savedDevice(names, saved);
}

// Сохранённое устройство из списка. Прежние версии запоминали имя с
// хвостом «[Microphone] via USB» — узнаём и такое.
function savedDevice(names, saved) {
  if (!saved) return "";
  if (names.includes(saved)) return saved;
  return names.find((name) => saved.startsWith(`${name} `)) ?? "";
}

// ---------------------------------------------------------------------------
// Вход в комнату: одно нажатие на карточку
// ---------------------------------------------------------------------------

// Карточка комнаты (цель входа): { key, server, title, meta, kind, password,
// disabled }. kind: "lan" — найдена в локальной сети, "recent" — входили
// раньше, "manual" — вход по адресу или коду из формы.
const join = {
  busy: null, // key карточки, к которой подключаемся
  prompt: null, // key карточки, у которой открыто поле пароля
  promptError: "",
  errors: new Map(), // key → текст ошибки под названием
  lan: [], // последний ответ поиска в локальной сети
  last: null, // последняя попытка: { target, password } — для «Доверять»
  // Запущены ли недавние комнаты: server → { state, at }, state — "online",
  // "offline" или "unknown" (проверить не удалось), at — когда проверили.
  status: new Map(),
  checking: null, // key карточки, которую перепроверяем перед входом
};

const LAN_REFRESH_MS = 5000;
let lanTimer = null;
let lanSearching = false;
let lanSearched = false;

async function refreshLan() {
  if (lanSearching || $("connect-view").hidden) return;
  lanSearching = true;
  try {
    const before = join.lan.map((s) => s.address);
    join.lan = await invoke("discover_servers");
    lanSearched = true;
    // Комната пропала из локальной сети — её прежний статус в недавних
    // устарел (скорее всего, её закрыли): проверяем сразу.
    const now = new Set(join.lan.map((s) => s.address));
    const gone = before.filter((a) => !now.has(a));
    if (gone.length) {
      for (const server of gone) forgetStatus(server);
      checkRecent({ force: true });
    }
    // Пока вводят пароль или идёт подключение, список не перестраиваем:
    // иначе пропадёт набранный текст.
    if (!join.prompt && !join.busy) renderJoin();
  } catch (e) {
    $("lan-status").hidden = false;
    $("lan-status").textContent = `Поиск не удался: ${errorText(e)}`;
  } finally {
    lanSearching = false;
  }
}

function startLanSearch() {
  clearInterval(lanTimer);
  clearInterval(roomCheckTimer);
  renderJoin();
  refreshLan();
  checkRecent({ force: true });
  lanTimer = setInterval(refreshLan, LAN_REFRESH_MS);
  roomCheckTimer = setInterval(checkRecent, ROOM_CHECK_TICK_MS);
}

function stopLanSearch() {
  clearInterval(lanTimer);
  clearInterval(roomCheckTimer);
  lanTimer = null;
  roomCheckTimer = null;
}

// Недавние комнаты проверяются в фоне: запущены ли они сейчас. По адресу
// проверка дешёвая (только QUIC-рукопожатие) — часто. По коду приглашения —
// чтение ящика ntfy.sh: сообщений из дневного лимита (250 на IP) не тратит, но
// у ntfy.sh есть и лимит запросов, поэтому реже. Сообщение («ты тут?» хосту)
// уходит, только когда человек сам нажал на комнату (см. stillOffline).
const ROOM_CHECK_TICK_MS = 15000;
const CHECK_EVERY_MS = { address: 15000, invite: 120000 };
// Не чаще этого даже при «срочной» проверке (открыли экран, вернулись в приложение).
const CHECK_MIN_MS = { address: 0, invite: 30000 };
// Старше этого ответ уже не показываем как правду — «Проверяю…».
const STATUS_STALE_MS = { address: 45000, invite: 300000 };
let roomCheckTimer = null;
// Когда комнату последний раз пробовали проверить, даже если не вышло: после
// неудачи (нет интернета, лимит ntfy.sh) не повторяем каждые 15 секунд.
const roomTried = new Map();
// Комнаты, проверка которых ещё идёт: каждая проверяется отдельно, чтобы
// быстрый ответ (по адресу) не ждал медленного (по коду приглашения).
const roomsChecking = new Set();

const checkKind = (server) => (isInvite(server) ? "invite" : "address");

function forgetStatus(server) {
  join.status.delete(server);
  roomTried.delete(server);
}

// Что показать на карточке: устаревший ответ — как «Проверяю…».
function shownStatus(server) {
  const status = join.status.get(server);
  if (!status) return "checking"; // ещё не проверяли — проверка вот-вот будет
  if (status.state === "unknown") return null;
  if (Date.now() - status.at > STATUS_STALE_MS[checkKind(server)]) return "checking";
  return status.state;
}

function setStatus(server, state) {
  const old = join.status.get(server);
  // Не смогли проверить (нет интернета, лимит ntfy) — свежий прежний ответ
  // лучше, чем никакого.
  if (state === "unknown" && old && old.state !== "unknown") {
    if (Date.now() - old.at <= STATUS_STALE_MS[checkKind(server)]) return;
  }
  join.status.set(server, { state, at: Date.now() });
  // Хост открыл комнату — старое «не запущена» под названием больше не верно.
  if (state !== "offline" && join.errors.get(server) === OFFLINE_TEXT) {
    join.errors.delete(server);
  }
}

// Какие недавние комнаты пора проверить. force — по событию (экран открыт,
// вернулись в приложение, комната пропала из сети): не ждём обычного срока.
function dueServers(force) {
  const found = new Set(join.lan.map((s) => s.address));
  const now = Date.now();
  return recentRooms()
    .map((r) => r.server)
    .filter((server) => !found.has(server) && !roomsChecking.has(server))
    .filter((server) => {
      const at = roomTried.get(server);
      if (at === undefined) return true;
      const kind = checkKind(server);
      return now - at >= (force ? CHECK_MIN_MS[kind] : CHECK_EVERY_MS[kind]);
    });
}

function checkRecent({ force = false } = {}) {
  // Окно свёрнуто — статус никто не видит, а запросы к ntfy.sh идут.
  if ($("connect-view").hidden || document.hidden) return;
  for (const server of dueServers(force)) checkRoom(server);
}

// Проверяет одну комнату и сразу показывает ответ.
async function checkRoom(server) {
  roomsChecking.add(server);
  roomTried.set(server, Date.now());
  let status = "unknown";
  try {
    [{ status }] = await invoke("check_rooms", { servers: [server] });
  } catch {}
  roomsChecking.delete(server);
  setStatus(server, status);
  if (!join.prompt && !join.busy) renderJoin();
}

// Вернулись в приложение (на телефоне таймеры, пока оно свёрнуто, стоят).
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) checkRecent({ force: true });
});

// Нажали на комнату, которая при последней проверке была не запущена:
// сначала быстро проверяем ещё раз, вместо долгой попытки подключиться.
// true — комната по-прежнему не запущена, входить не нужно.
async function stillOffline(target) {
  join.busy = target.key;
  join.checking = target.key;
  join.errors.delete(target.key);
  renderJoin();
  let status = "unknown";
  try {
    roomTried.set(target.server, Date.now());
    [{ status }] = await invoke("check_rooms", { servers: [target.server], ask: true });
  } catch {}
  setStatus(target.server, status);
  join.busy = null;
  join.checking = null;
  if (status !== "offline") return false;
  join.errors.set(target.key, OFFLINE_TEXT);
  renderJoin();
  return true;
}

const OFFLINE_TEXT = "Комната сейчас не запущена. Попросите хоста открыть её.";

$("lan-refresh").addEventListener("click", () => {
  $("lan-status").hidden = false;
  $("lan-status").textContent = "Ищу комнаты в сети…";
  refreshLan();
});

// Недавние комнаты: куда удачно входили по коду или адресу. Новые — первыми.
const RECENT_MAX = 5;

function recentRooms() {
  const recent = store.get("recentRooms", null);
  if (recent) return recent;
  // Прежние версии помнили только последний адрес в поле «Сервер».
  const server = store.get("server", "");
  return server ? [{ server, title: null, at: null }] : [];
}

function rememberRecent(target, joined) {
  const host = joined.peers.find((p) => p.admin);
  const title = target.kind === "lan" ? target.title : host ? `Комната — ${host.name}` : null;
  const old = recentRooms().find((r) => r.server === target.server);
  const entry = { server: target.server, title: title ?? old?.title ?? null, at: Date.now() };
  const rest = recentRooms().filter((r) => r.server !== target.server);
  store.set("recentRooms", [entry, ...rest].slice(0, RECENT_MAX));
}

function forgetRecent(server) {
  store.set("recentRooms", recentRooms().filter((r) => r.server !== server));
  forgetPassword(server);
  renderJoin();
}

function whenText(at) {
  if (!at) return "";
  const day = (t) => new Date(t).setHours(0, 0, 0, 0);
  const days = Math.round((day(Date.now()) - day(at)) / 86400000);
  if (days === 0) return "сегодня";
  if (days === 1) return "вчера";
  return new Date(at).toLocaleDateString("ru-RU", { day: "numeric", month: "long" });
}

function joinTargets() {
  const lan = join.lan.map((s) => ({
    key: s.address,
    server: s.address,
    title: s.name,
    meta: s.compatible
      ? [s.password ? "по паролю" : null, s.host, `${s.users} чел.`].filter(Boolean).join(" · ")
      : `${s.host} · другая версия программы`,
    kind: "lan",
    password: s.password,
    disabled: !s.compatible,
  }));
  const found = new Set(lan.map((t) => t.server));
  const recent = recentRooms()
    .filter((r) => !found.has(r.server))
    .map((r) => {
      const invite = isInvite(r.server);
      return {
        key: r.server,
        server: r.server,
        title: r.title ?? (invite ? "Комната по приглашению" : r.server),
        meta: [invite ? "по коду приглашения" : r.title ? r.server : "по адресу", whenText(r.at)]
          .filter(Boolean)
          .join(" · "),
        kind: "recent",
        password: false,
        disabled: false,
      };
    });
  return [...recent, ...lan];
}

function renderJoin() {
  const list = $("join-list");
  list.innerHTML = "";
  const targets = joinTargets();
  const busy = Boolean(join.busy) || roomsBusy;
  const recent = targets.filter((t) => t.kind === "recent");
  // Свёрнутый список раскрываем, пока в одну из комнат входят или вводят пароль.
  const active = recent.some((t) => t.key === join.busy || t.key === join.prompt);
  const collapsed = store.get("recentCollapsed", false) && !active;
  if (recent.length) list.append(recentToggle(recent, collapsed, active));
  for (const target of targets) {
    if (collapsed && target.kind === "recent") continue;
    list.append(joinCard(target, busy));
  }

  const status = $("lan-status");
  status.hidden = join.lan.length > 0;
  status.textContent = !lanSearched
    ? "Ищу комнаты в сети…"
    : targets.length
      ? "Других комнат в сети не видно."
      : "В сети комнат не видно. Друг зовёт через интернет — войдите по его коду приглашения.";
  $("manual-open").disabled = busy;
  $("connect-btn").disabled = busy;
  $("connect-btn").textContent =
    join.busy === "manual"
      ? isInvite($("server").value)
        ? "Связываюсь с хостом через интернет…"
        : "Подключение…"
      : "Войти";
  renderRooms();

  const input = list.querySelector(".join-pass input");
  if (input && document.activeElement !== input) input.focus();
}

// Заголовок «Сохранённые»: сворачивает недавние комнаты. В свёрнутом виде
// статусы комнат видны точками прямо в заголовке.
function recentToggle(recent, collapsed, active) {
  const li = document.createElement("li");
  li.className = "recent-head";
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "recent-toggle";
  btn.classList.toggle("collapsed", collapsed);
  btn.disabled = active;
  btn.setAttribute("aria-expanded", String(!collapsed));
  const label = document.createElement("span");
  label.textContent = `Сохранённые · ${recent.length}`;
  btn.append(icon("chevron"), label);
  if (collapsed) {
    const dots = document.createElement("span");
    dots.className = "recent-dots";
    for (const t of recent) {
      const status = shownStatus(t.server);
      if (!status) continue;
      const dot = document.createElement("span");
      dot.className = `recent-dot ${status}`;
      dot.title = `${t.title}: ${ROOM_STATE[status].toLowerCase()}`;
      dots.append(dot);
    }
    btn.append(dots);
  }
  btn.addEventListener("click", () => {
    store.set("recentCollapsed", !collapsed);
    renderJoin();
  });
  li.append(btn);
  return li;
}

const ROOM_STATE = { online: "Запущена", offline: "Не запущена", checking: "Проверяю…" };

function joinCard(target, busy) {
  const li = document.createElement("li");
  li.className = "join-item";
  const connecting = join.busy === target.key;
  const prompting = join.prompt === target.key;
  li.classList.toggle("busy", connecting);
  li.classList.toggle("open", prompting);

  const card = document.createElement("button");
  card.type = "button";
  card.className = "lan-item join-card";
  card.disabled = target.disabled || (busy && !connecting);
  const ico = document.createElement("span");
  ico.className = `join-ico ${target.kind}`;
  ico.append(icon(target.kind === "lan" ? "wifi" : "history"));
  const text = document.createElement("span");
  text.className = "lan-text";
  const name = document.createElement("span");
  name.className = "lan-name";
  name.textContent = target.title;
  const meta = document.createElement("span");
  meta.className = "lan-meta";
  const error = join.errors.get(target.key);
  const status = target.kind === "recent" ? shownStatus(target.server) : null;
  if (status) {
    ico.classList.add(status);
    // Статус — только цветом точки, словами — во всплывающей подсказке.
    ico.title = ROOM_STATE[status];
    const dot = document.createElement("span");
    dot.className = "join-dot";
    ico.append(dot);
  }
  if (connecting) {
    meta.textContent =
      join.checking === target.key
        ? "Проверяю, запущена ли комната…"
        : isInvite(target.server)
          ? "Связываюсь с хостом через интернет…"
          : "Подключение…";
  } else if (error) {
    meta.textContent = error;
    meta.classList.add("error");
  } else {
    if (target.password) meta.append(icon("lock"));
    meta.append(target.meta);
  }
  text.append(name, meta);
  const end = document.createElement("span");
  end.className = connecting ? "spinner join-spinner" : "join-go";
  if (!connecting && !target.disabled) end.append(icon("chevron-right"));
  card.append(ico, text, end);
  card.addEventListener("click", () => {
    if (prompting) return li.querySelector(".join-pass input")?.focus();
    joinTarget(target);
  });
  li.append(card);

  if (target.kind === "recent" && !busy && !prompting) {
    li.classList.add("has-forget");
    const forget = document.createElement("button");
    forget.type = "button";
    forget.className = "icon-btn join-forget";
    forget.setAttribute("aria-label", "Убрать из недавних");
    forget.title = "Убрать из недавних";
    forget.disabled = busy;
    forget.append(icon("close"));
    forget.addEventListener("click", () => forgetRecent(target.server));
    li.append(forget);
  }
  if (prompting) li.append(passwordPanel(target, connecting));
  return li;
}

// Поле пароля прямо в карточке комнаты.
function passwordPanel(target, connecting) {
  const form = document.createElement("form");
  form.className = "join-pass";
  form.autocomplete = "off";
  const input = document.createElement("input");
  input.type = "password";
  input.maxLength = 64;
  input.placeholder = "Пароль комнаты";
  input.setAttribute("aria-label", "Пароль комнаты");
  input.value = savedPasswords()[target.server] ?? "";
  form.append(input);
  if (join.promptError) {
    const error = document.createElement("p");
    error.className = "error";
    error.textContent = join.promptError;
    form.append(error);
  }
  const row = document.createElement("div");
  row.className = "join-pass-actions";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.textContent = "Отмена";
  cancel.addEventListener("click", closePrompt);
  const submit = document.createElement("button");
  submit.type = "submit";
  submit.className = "primary";
  submit.textContent = "Войти";
  row.append(cancel, submit);
  form.append(row);
  for (const el of [input, cancel, submit]) el.disabled = connecting;
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    if (!input.value) return input.focus();
    joinTarget(target, input.value);
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closePrompt();
  });
  return form;
}

function closePrompt() {
  join.prompt = null;
  join.promptError = "";
  renderJoin();
}

// Имя обязательно: без него не войти ни в одну комнату.
function checkName() {
  const input = $("name");
  if (input.value.trim()) return true;
  input.focus();
  input.reportValidity();
  return false;
}

// Читает и запоминает имя и устройства.
function formValues() {
  const values = {
    name: $("name").value.trim(),
    input: $("input").value,
    output: $("output").value,
  };
  for (const [key, value] of Object.entries(values)) store.set(key, value);
  return values;
}

// Вход: сразу, а если комната с паролем — с запомненным или введённым.
async function joinTarget(target, typed = null) {
  if (join.busy || roomsBusy || !checkName()) return;
  // Повторное нажатие после «не запущена» — всё-таки пробуем войти: хост
  // старой версии не отвечает на проверку, но впустить может.
  if (
    target.kind === "recent" &&
    shownStatus(target.server) === "offline" &&
    join.errors.get(target.key) !== OFFLINE_TEXT
  ) {
    if (await stillOffline(target)) return;
  }
  const manual = target.kind === "manual";
  const password = typed ?? savedPasswords()[target.server] ?? null;
  if (target.password && !password) {
    join.prompt = target.key;
    join.promptError = "";
    return renderJoin();
  }
  const { name, input, output } = formValues();
  join.busy = target.key;
  join.errors.delete(target.key);
  join.last = { target, password };
  $("cert-warning").hidden = true;
  $("manual-error").hidden = true;
  renderJoin();
  prepareAudio();
  try {
    avatars.clear();
    const joined = await invoke("connect", {
      args: { server: target.server, name, input, output, password: password || null },
    });
    if (password) rememberPassword(target.server, password);
    rememberRecent(target, joined);
    // Пока мы в комнате, она не проверяется: после выхода — проверить заново.
    forgetStatus(target.server);
    join.prompt = null;
    join.promptError = "";
    if (manual) closeManual();
    enterRoom(target.server, name, joined, recentRooms()[0]?.title);
  } catch (e) {
    androidBridge?.callEnded();
    notifiedMuted = null;
    if (e?.kind === "certificateChanged") {
      showCertWarning(e);
    } else if (needsPassword(e)) {
      forgetPassword(target.server);
      const text = errorText(e).includes(WRONG_PASSWORD)
        ? "Неверный пароль — проверьте у хоста."
        : "В эту комнату входят по паролю.";
      if (manual) {
        showManualError(text);
        $("manual-pass").value = "";
        $("manual-pass").focus();
      } else {
        join.prompt = target.key;
        join.promptError = text;
      }
    } else if (manual) {
      showManualError(errorText(e));
    } else {
      join.prompt = null;
      join.errors.set(target.key, errorText(e));
      // Не вошли — заодно обновим, запущена ли комната.
      if (target.kind === "recent") {
        forgetStatus(target.server);
        setTimeout(() => checkRecent({ force: true }));
      }
    }
  } finally {
    join.busy = null;
    renderJoin();
  }
}

// Вход по адресу или коду приглашения.
function openManual() {
  $("manual-open").hidden = true;
  $("manual-form").hidden = false;
  $("manual-error").hidden = true;
  $("server").focus();
}

function closeManual() {
  $("manual-form").hidden = true;
  $("manual-open").hidden = false;
  $("server").value = "";
  $("manual-pass").value = "";
  $("manual-error").hidden = true;
}

function showManualError(text) {
  $("manual-error").textContent = text;
  $("manual-error").hidden = false;
}

$("manual-open").addEventListener("click", openManual);
$("manual-close").addEventListener("click", closeManual);
$("manual-form").addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeManual();
});
$("manual-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const server = $("server").value.trim();
  if (!server) return $("server").focus();
  joinTarget(
    { key: "manual", server, title: null, kind: "manual", password: false },
    $("manual-pass").value || null,
  );
});

// Причины отказа сервера (vwf_proto::PASSWORD_REQUIRED_REASON и т.д.).
const PASSWORD_REQUIRED = "в эту комнату входят по паролю";
const WRONG_PASSWORD = "неверный пароль комнаты";

function needsPassword(e) {
  const text = errorText(e);
  return text.includes(PASSWORD_REQUIRED) || text.includes(WRONG_PASSWORD);
}

// Пароли комнат запоминаются по адресу сервера (или коду приглашения),
// чтобы не вводить их при каждом входе.
function savedPasswords() {
  return store.get("roomPasswords", {});
}

function rememberPassword(server, password) {
  store.set("roomPasswords", { ...savedPasswords(), [server]: password });
}

function forgetPassword(server) {
  const passwords = savedPasswords();
  delete passwords[server];
  store.set("roomPasswords", passwords);
}

function isInvite(text) {
  return text.trim().startsWith("poi-");
}

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
    if (join.last) joinTarget(join.last.target, join.last.password);
  } catch (e) {
    toast(`Не удалось запомнить сертификат: ${errorText(e)}`);
  }
});

// Аватар рядом с именем — как его увидят в комнате.
function renderWho() {
  paintAvatar($("who-avatar"), $("name").value.trim() || "?", myAvatar);
}

$("name").addEventListener("input", renderWho);

// ---------------------------------------------------------------------------
// Свои комнаты: сервер запускается прямо в программе
// ---------------------------------------------------------------------------

let myRooms = [];
let roomsBusy = false;
let roomStarting = null; // id своей комнаты, которая сейчас запускается

async function loadRooms() {
  try {
    myRooms = await invoke("list_rooms");
  } catch (e) {
    toast(`Не удалось загрузить список комнат: ${errorText(e)}`);
  }
  renderRooms();
}

function roomMeta(room) {
  const parts = [
    room.lan && room.internet
      ? "сеть + интернет"
      : room.lan
        ? "видна в сети"
        : room.internet
          ? "только интернет"
          : "скрыта из сети",
  ];
  if (room.password) parts.push("с паролем");
  if (room.maxPeers) parts.push(`до ${room.maxPeers} чел.`);
  return parts.join(" · ");
}

function renderRooms() {
  const list = $("my-rooms");
  list.innerHTML = "";
  const busy = roomsBusy || Boolean(join.busy);
  for (const room of myRooms) {
    const li = document.createElement("li");
    const starting = room.id === roomStarting;
    li.classList.toggle("busy", starting);
    const open = document.createElement("button");
    open.type = "button";
    open.className = "lan-item";
    open.disabled = busy;
    const badge = document.createElement("span");
    badge.className = "room-badge";
    badge.textContent = initial(room.name);
    const text = document.createElement("span");
    text.className = "lan-text";
    const name = document.createElement("span");
    name.className = "lan-name";
    name.textContent = room.name;
    const meta = document.createElement("span");
    meta.className = "lan-meta";
    const error = join.errors.get(`room:${room.id}`);
    meta.textContent = starting ? "Запуск сервера…" : error ?? roomMeta(room);
    meta.classList.toggle("error", !starting && Boolean(error));
    text.append(name, meta);
    open.append(badge, text);
    open.addEventListener("click", () => openRoom(room));
    li.append(open, roomMenu(room, li));
    list.append(li);
  }
  $("new-room-btn").disabled = busy;
  renderRoomsHint();
}

// Телефон: своя комната доступна друзьям только из Wi-Fi (через мобильный
// интернет к телефону почти никогда не подключиться снаружи).
function renderRoomsHint() {
  if (!androidBridge?.networkType) return;
  const wifi = androidBridge.networkType() === "wifi";
  const hint = $("rooms-hint");
  hint.classList.toggle("warning-text", !wifi);
  hint.textContent = wifi
    ? "Комната на телефоне работает, пока он в Wi-Fi: друзья из той же сети найдут её в списке, остальные — по коду приглашения."
    : "Телефон не в Wi-Fi: через мобильный интернет друзья к вашей комнате почти никогда не подключатся. Подключитесь к Wi-Fi.";
}

// Меню комнаты: переименовать, удалить.
function roomMenu(room, li) {
  const wrap = document.createElement("div");
  wrap.className = "menu-wrap";
  const button = toggleButton(null, icon("more"), false, "Действия с комнатой", (event) => {
    event.stopPropagation();
    const open = !menu.hidden;
    closeMenus();
    menu.hidden = open;
  });
  button.removeAttribute("title");
  button.setAttribute("aria-label", "Действия с комнатой");
  button.disabled = roomsBusy || Boolean(join.busy);
  const menu = document.createElement("div");
  menu.className = "menu";
  menu.hidden = true;

  const settings = document.createElement("button");
  settings.type = "button";
  settings.textContent = "Настройки";
  settings.addEventListener("click", () => {
    closeMenus();
    openRoomSheet(room);
  });
  menu.append(settings);

  // Удалить — нужно подтвердить вторым нажатием. Последнюю комнату не удалить.
  if (myRooms.length > 1) {
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "danger-item";
    remove.textContent = "Удалить";
    remove.title = "Код приглашения этой комнаты перестанет работать";
    remove.addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!remove.classList.contains("armed")) {
        remove.classList.add("armed");
        remove.textContent = "Точно удалить? Код перестанет работать";
        return;
      }
      closeMenus();
      try {
        await invoke("delete_room", { id: room.id });
        myRooms = myRooms.filter((r) => r.id !== room.id);
      } catch (e) {
        toast(errorText(e));
      }
      renderRooms();
    });
    menu.append(remove);
  }
  wrap.append(button, menu);
  return wrap;
}

// Окно «Новая комната» / «Настройки комнаты». Новая комната получает
// параметры последней (кроме названия и пароля), чтобы не настраивать заново.
let editingRoom = null;

function openRoomSheet(room) {
  editingRoom = room;
  const base = room ?? myRooms[myRooms.length - 1] ?? {};
  $("rs-title").textContent = room ? "Настройки комнаты" : "Новая комната";
  $("rs-save").textContent = room ? "Сохранить" : "Создать";
  $("rs-name").value = room?.name ?? "";
  $("rs-password").value = room?.password ?? "";
  showRoomPassword(false);
  const max = String(base.maxPeers ?? 0);
  $("rs-max").value = [...$("rs-max").options].some((o) => o.value === max) ? max : "0";
  $("rs-lan").checked = base.lan ?? true;
  $("rs-internet").checked = base.internet ?? false;
  $("rs-note").hidden = !(room && call.hostRoom?.id === room.id);
  $("room-sheet").hidden = false;
  $("rs-name").focus();
}

function closeRoomSheet() {
  $("room-sheet").hidden = true;
  editingRoom = null;
}

function showRoomPassword(show) {
  $("rs-password").type = show ? "text" : "password";
  $("rs-show").classList.toggle("active", show);
  $("rs-show").setAttribute("aria-label", show ? "Скрыть пароль" : "Показать пароль");
}

$("rs-show").addEventListener("click", () => showRoomPassword($("rs-password").type === "password"));
$("rs-cancel").addEventListener("click", closeRoomSheet);
$("rs-close").addEventListener("click", closeRoomSheet);
$("room-sheet").addEventListener("click", (event) => {
  if (event.target === $("room-sheet")) closeRoomSheet();
});

$("room-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!$("rs-name").value.trim()) return $("rs-name").reportValidity();
  const settings = {
    name: $("rs-name").value,
    password: $("rs-password").value,
    maxPeers: Number($("rs-max").value),
    lan: $("rs-lan").checked,
    internet: $("rs-internet").checked,
  };
  try {
    if (editingRoom) {
      const updated = await invoke("update_room", { id: editingRoom.id, settings });
      myRooms = myRooms.map((r) => (r.id === updated.id ? updated : r));
      // Открытая комната: обновить окно «Пригласить друзей».
      if (call.hostRoom?.id === updated.id) {
        call.hostRoom = updated;
        renderInvitePassword();
        $("room-lan").checked = updated.lan;
        $("internet").checked = updated.internet;
        setInternet(updated.internet);
      }
    } else {
      myRooms.push(await invoke("create_room", { settings }));
    }
  } catch (e) {
    toast(errorText(e));
    return;
  }
  closeRoomSheet();
  renderRooms();
});

$("new-room-btn").addEventListener("click", () => openRoomSheet(null));

// Открываем свою комнату: запускаем сервер и подключаемся к нему сами.
async function openRoom(room) {
  if (roomsBusy || join.busy || !checkName()) return;
  const { name, input, output } = formValues();
  roomsBusy = true;
  roomStarting = room.id;
  join.errors.delete(`room:${room.id}`);
  renderJoin();
  let hosted = null;
  prepareAudio();
  try {
    hosted = await invoke("start_server", { roomId: room.id });
    avatars.clear();
    const joined = await invoke("connect", {
      args: { server: hosted.localAddress, name, input, output },
    });
    call.hosting = hosted.info;
    call.hostRoom = hosted.room;
    enterRoom(hosted.localAddress, name, joined);
  } catch (e) {
    androidBridge?.callEnded();
    notifiedMuted = null;
    if (hosted) await invoke("stop_server").catch(() => {});
    join.errors.set(`room:${room.id}`, errorText(e));
  } finally {
    roomsBusy = false;
    roomStarting = null;
    renderJoin();
  }
}

// ---------------------------------------------------------------------------
// Комната
// ---------------------------------------------------------------------------

// title — название чужой комнаты, если оно известно (для уведомлений Android).
function enterRoom(server, name, joined, title = null) {
  call.server = server;
  call.title = call.hosting?.name ?? title ?? "";
  // Android: уведомление «вы в комнате» держит звонок, когда программа свёрнута.
  androidBridge?.callStarted(call.title);
  notifiedMuted = false;
  setReconnecting(false);
  applyJoined(joined);
  renderHostControls();
  const tab = store.get("roomTab", "people");
  selectTab(tab);
  showTab(tab);
  showView("room");
  stopLanSearch();
  clearInterval(call.statusTimer);
  call.statusTimer = setInterval(refreshStatus, 100);
  clearInterval(call.qualityTimer);
  call.quality.clear();
  qualityCounters.clear();
  call.qualityTimer = setInterval(refreshQuality, 2000);
  refreshCameraPill();
}

// Сведения о комнате после (пере)подключения.
function applyJoined(joined) {
  const server = call.server;
  call.me = joined.me;
  call.peers = new Map(joined.peers.map((p) => [p.id, p]));
  checkNewerApp();
  $("room-title").textContent = call.hosting?.name ?? "Комната";
  $("server-label").textContent = call.hosting
    ? "ваш сервер"
    : isInvite(server)
      ? "через интернет"
      : server;
  $("server-code").textContent = joined.serverCode;
  // Хосту код сервера виден в окне «Пригласить друзей», а в шапке — место для «вход закрыт».
  $("server-code").hidden = Boolean(call.hosting);
  showDevice("input", joined.inputDevice);
  showDevice("output", joined.outputDevice);
  // После (пере)подключения демонстрация не идёт: звонок новый.
  setSharing(false);
  // Камеру после переподключения включаем снова (на телефоне Kotlin и не
  // переставал снимать) — плитки начнут заново с новым звонком.
  clearCameraTiles();
  if (call.camera) {
    invoke("start_camera", { id: call.cameraId }).catch((e) => {
      call.camera = false;
      androidBridge?.stopCamera();
      renderPeers();
      toast(capitalized(errorText(e)));
    });
  }
  call.chat = [];
  for (const message of [...joined.chat, ...call.chatPending]) addMessage(message, false);
  call.chatPending = [];
  renderChat(true);
  renderPeers();
  for (const [id, peer] of call.peers) applySavedVolume(id, peer.name);
}

// Связь пропала, программа пытается вернуться в комнату.
function setReconnecting(on, attempt = 0) {
  call.reconnecting = on;
  $("room-view").classList.toggle("reconnecting", on);
  $("reconnect-banner").hidden = !on;
  if (on) {
    $("reconnect-text").textContent =
      attempt > 1
        ? `Связь пропала. Возвращаюсь в комнату… (попытка ${attempt})`
        : "Связь пропала. Возвращаюсь в комнату…";
  }
}

listen("call-reconnecting", ({ payload }) => {
  if (!call.me) return; // уже вышли из комнаты
  setReconnecting(true, payload.attempt);
});

listen("call-reconnected", ({ payload }) => {
  if (!call.reconnecting) return; // пока шло переподключение, нажали «Выйти»
  setReconnecting(false);
  applyJoined(payload);
  toast("Связь восстановлена");
});

function leaveRoom(reason) {
  closeScreenView();
  if (call.camera) androidBridge?.stopCamera();
  call.camera = false;
  call.cameraViewers = 0;
  clearCameraTiles();
  androidBridge?.callEnded();
  notifiedMuted = null;
  setReconnecting(false);
  closeShareSheet();
  setSharing(false);
  call.chat = [];
  call.chatPending = [];
  setUnread(0);
  call.hosting = null;
  call.hostRoom = null;
  call.me = null;
  hideMutedHint();
  closeMenus();
  closeInvite();
  stopInternetPolling();
  clearInterval(call.statusTimer);
  call.statusTimer = null;
  clearInterval(call.qualityTimer);
  call.qualityTimer = null;
  call.quality.clear();
  call.peers.clear();
  showView("connect");
  // Комнату, из которой вышли, могли как раз закрыть: проверяем её сразу и
  // ещё раз чуть позже — сервер мог ещё отвечать, пока закрывался.
  const left = call.server;
  if (left) {
    forgetStatus(left);
    setTimeout(() => {
      forgetStatus(left);
      checkRecent({ force: true });
    }, 4000);
  }
  startLanSearch();
  loadRooms(); // в комнате могли включить интернет или скрыть её из сети
  if (reason) toast(reason);
}

async function leave() {
  const wasHosting = Boolean(call.hosting);
  try {
    await invoke("disconnect");
    if (wasHosting) await invoke("stop_server");
  } finally {
    leaveRoom();
  }
}

// Хосту закрытие комнаты отключает всех, поэтому сначала спрашиваем —
// прямо над кнопкой, как меню. Повторное нажатие на кнопку закрывает вопрос.
$("leave-btn").addEventListener("click", (event) => {
  if (!call.hosting) return leave();
  event.stopPropagation();
  const open = $("leave-menu").hidden;
  closeMenus();
  closeInvite();
  if (!open) return;
  $("leave-menu").hidden = false;
  $("leave-btn").setAttribute("aria-expanded", "true");
  $("leave-confirm").focus();
});

$("leave-menu").addEventListener("click", (event) => event.stopPropagation());
$("leave-confirm").addEventListener("click", () => {
  closeMenus();
  leave();
});
$("leave-cancel").addEventListener("click", closeMenus);

// Кнопки шапки и окно «Пригласить друзей» для своей комнаты.
function renderHostControls() {
  const hosting = Boolean(call.hosting);
  const leaveText = hosting ? "Закрыть комнату" : "Выйти";
  $("leave-btn").dataset.tip = leaveText;
  $("leave-btn").setAttribute("aria-label", leaveText);
  $("lock-btn").hidden = !hosting;
  $("invite-btn").hidden = !hosting;
  if (!hosting) return;
  showLocked(false); // новая комната всегда с открытым входом
  $("host-code").textContent = call.hosting.code;
  $("room-lan").checked = call.hostRoom.lan;
  // Раньше «доступ через интернет» запоминался для всех комнат сразу.
  const legacy = call.hostRoom.id === "default" && store.get("internet", false);
  try {
    localStorage.removeItem("internet");
  } catch {
    // Не страшно.
  }
  const internet = call.hostRoom.internet || legacy;
  $("internet").checked = internet;
  if (internet) setInternet(true);
  else renderInternet(null);
  renderAddress();
  renderInvitePassword();
}

// Пароль своей комнаты в окне «Пригласить друзей» — чтобы передать друзьям.
function renderInvitePassword() {
  const password = call.hostRoom?.password ?? "";
  $("invite-password").hidden = !password;
  $("invite-password-value").textContent = password;
}

$("invite-password-copy").addEventListener("click", () =>
  copyText($("invite-password-value").textContent),
);

$("invite-settings").addEventListener("click", () => {
  const room = myRooms.find((r) => r.id === call.hostRoom?.id);
  if (!room) return;
  closeInvite();
  openRoomSheet(room);
});

function showLocked(locked) {
  const button = $("lock-btn");
  button.setAttribute("aria-pressed", String(locked));
  button.title = locked ? "Новые участники не войдут. Нажмите, чтобы открыть вход" : "Нажмите, чтобы никого больше не пускать";
  button.replaceChildren(icon(locked ? "lock" : "lock-open"), locked ? "вход закрыт" : "вход открыт");
}

$("lock-btn").addEventListener("click", async () => {
  const locked = $("lock-btn").getAttribute("aria-pressed") !== "true";
  try {
    await invoke("set_lock", { locked });
  } catch (e) {
    return toast(errorText(e));
  }
  showLocked(locked);
  toast(locked ? "Вход закрыт: новые участники не войдут" : "Вход снова открыт");
});

// Адрес для друзей: у компьютера их бывает много (Ethernet, Wi-Fi, Tailscale,
// виртуальные сети), показываем один — выбранный в прошлый раз или Tailscale.
function chosenAddress() {
  const addresses = call.hosting?.addresses ?? [];
  const saved = store.get("inviteInterface", null);
  return (
    addresses.find((a) => a.interface === saved) ??
    addresses.find((a) => /tailscale/i.test(a.interface)) ??
    addresses[0] ??
    null
  );
}

function renderAddress() {
  const chosen = chosenAddress();
  $("address-iface").textContent = chosen ? chosen.interface : "";
  $("address-value").textContent = chosen ? chosen.address : "Компьютер не подключён к сети";
  $("address-copy").disabled = !chosen;
  const button = $("address-btn");
  button.disabled = (call.hosting?.addresses.length ?? 0) < 2;
  button.setAttribute("aria-label", button.disabled ? "Адрес" : "Выбрать другой адрес");
  button.setAttribute("aria-expanded", "false");

  const menu = $("address-menu");
  menu.hidden = true;
  menu.innerHTML = "";
  for (const address of call.hosting?.addresses ?? []) {
    const item = document.createElement("button");
    item.type = "button";
    item.classList.toggle("current", address === chosen);
    const check = icon("check");
    check.classList.add("check");
    const iface = document.createElement("span");
    iface.className = "host-iface";
    iface.textContent = address.interface;
    const value = document.createElement("span");
    value.className = "host-address label";
    value.textContent = address.address;
    item.append(check, iface, value);
    item.addEventListener("click", () => {
      store.set("inviteInterface", address.interface);
      renderAddress();
    });
    menu.append(item);
  }
}

$("address-btn").addEventListener("click", (event) => {
  event.stopPropagation();
  const open = $("address-menu").hidden;
  closeMenus();
  $("address-menu").hidden = !open;
  $("address-btn").setAttribute("aria-expanded", String(open));
});

$("address-copy").addEventListener("click", () => {
  const chosen = chosenAddress();
  if (chosen) copyText(chosen.address);
});

function openInvite() {
  renderAddress();
  $("invite-sheet").hidden = false;
}

function closeInvite() {
  $("invite-sheet").hidden = true;
}

$("invite-btn").addEventListener("click", openInvite);
$("invite-close").addEventListener("click", closeInvite);
$("invite-sheet").addEventListener("click", (event) => {
  if (event.target === $("invite-sheet")) closeInvite();
});

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

// Что сейчас написано в уведомлении звонка Android; null — звонка нет.
let notifiedMuted = null;

// Нижняя панель: кто вы, состояние и иконки управления.
function renderDock() {
  const me = call.me;
  if (!me) return;
  paintAvatar($("me-avatar"), me.name, myAvatar);
  $("me-name").textContent = me.name;

  const status = $("me-status");
  status.className = "me-status";
  if (me.serverMuted) {
    status.textContent = "заглушены хостом";
    status.classList.add("off");
  } else if (me.muted || me.deafened) {
    status.textContent = me.deafened ? "звук выключен" : "микрофон выключен";
    status.classList.add("off");
  } else if (call.sharing) {
    status.textContent = `показываете экран · смотрят: ${call.viewers}`;
    status.classList.add("live");
  } else if (call.camera) {
    status.textContent = `камера · видят: ${call.cameraViewers}`;
    status.classList.add("live");
  } else {
    const key = store.get("pttKey", null);
    status.textContent = {
      voice: "активация голосом",
      always: "микрофон открыт",
      ptt: key ? `рация: ${keyLabel(key)}` : "рация: клавиша не назначена",
    }[currentMode];
  }

  // Android: кнопка и строка в уведомлении звонка — как микрофон здесь.
  if (androidBridge && notifiedMuted !== null && notifiedMuted !== me.muted) {
    notifiedMuted = me.muted;
    androidBridge.setMuted(me.muted);
  }

  const mic = $("mic-btn");
  mic.replaceChildren(icon("mic", me.muted));
  mic.classList.toggle("off", me.muted);
  mic.dataset.tip = me.muted ? "Включить микрофон" : "Выключить микрофон";
  const sound = $("sound-btn");
  sound.replaceChildren(icon("headphones", me.deafened));
  sound.classList.toggle("off", me.deafened);
  sound.dataset.tip = me.deafened ? "Включить звук" : "Выключить звук";
  const cameraBtn = $("camera-btn");
  cameraBtn.replaceChildren(icon("camera", !call.camera));
  cameraBtn.classList.toggle("active", call.camera);
  cameraBtn.dataset.tip = call.camera ? "Выключить камеру" : "Включить камеру";
  const share = $("share-btn");
  share.classList.toggle("active", call.sharing);
  share.dataset.tip = call.sharing
    ? `Остановить показ (смотрят: ${call.viewers})`
    : "Показать экран";
}

$("mic-btn").addEventListener("click", () => setMuted(!call.me.muted));
$("sound-btn").addEventListener("click", () => setDeafened(!call.me.deafened));

// Окно «Голос»: режим, порог, клавиша рации, шумоподавление, усиление.
$("voice-btn").addEventListener("click", (event) => {
  event.stopPropagation();
  const menu = $("voice-menu");
  const open = !menu.hidden;
  closeMenus();
  menu.hidden = open;
});
$("voice-menu").addEventListener("click", (event) => event.stopPropagation());

function renderPeers() {
  // Участник закончил показ (выключил камеру) или ушёл — закрываем просмотр.
  if (sv.view) {
    const peer = sv.peer === call.me?.id ? { camera: call.camera } : call.peers.get(sv.peer);
    if (!(sv.kind === "camera" ? peer?.camera : peer?.sharing)) closeScreenView();
  }
  renderDock();
  const list = $("peers");
  // Открытое меню человека остаётся открытым, когда список перерисовался.
  const openMenu = list.querySelector(".peer-menu:not([hidden])")?.closest(".peer")?.dataset.id;
  list.innerHTML = "";
  list.append(peerItem(call.me, true));
  for (const peer of [...call.peers.values()].sort((a, b) => a.id - b.id)) {
    list.append(peerItem(peer, false));
  }
  if (openMenu) list.querySelector(`.peer[data-id="${openMenu}"]`)?.click();
  $("people-count").textContent = String(call.peers.size + 1);
  if (call.peers.size === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = call.hosting
      ? "Пока никого. Позовите друзей кнопкой «Пригласить» вверху."
      : "Пока никого нет. Позовите друзей!";
    list.append(empty);
  }
  pruneCameraTiles();
}

function peerItem(peer, isMe) {
  const li = document.createElement("li");
  li.className = "peer";
  li.dataset.id = peer.id;
  if (isMe) li.classList.add("me");

  const avatar = document.createElement("div");
  avatar.className = "avatar";
  paintAvatar(avatar, peer.name, isMe ? myAvatar : avatars.get(peer.id));
  // Камера — вместо аватарки: в списке в кружке, в плитке на всю плитку.
  const camera = (isMe ? call.camera : peer.camera) ? cameraTile(peer.id, isMe) : null;
  if (camera) {
    li.classList.add("has-camera");
    avatar.classList.add("has-camera");
    // Своя камера — как в зеркале, но основная камера телефона — как есть.
    camera.canvas.classList.toggle("mirror", isMe && call.cameraId !== "back");
  }

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
  const nameEl = document.createElement("span");
  nameEl.className = "name";
  nameEl.textContent = peer.name;
  title.append(nameEl);
  if (isMe) {
    const you = document.createElement("span");
    you.className = "you";
    you.textContent = "вы";
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
  if (peer.sharing) {
    const badge = document.createElement("span");
    badge.className = "badge live";
    badge.textContent = "в эфире";
    badge.title = isMe ? "Вы показываете экран" : "Показывает экран";
    title.append(badge);
  }
  const side = document.createElement("div");
  side.className = "peer-side";
  if (peer.sharing) side.append(watchButton(peer, isMe));
  // Статус — просто значки; свои микрофон и звук переключаются внизу.
  if (peer.muted) side.append(statusIcon(icon("mic", true), isMe ? "Микрофон выключен" : "Выключил микрофон"));
  if (peer.deafened) side.append(statusIcon(icon("headphones", true), isMe ? "Звук выключен" : "Выключил звук"));
  if (!isMe) side.append(qualityIcon(peer.id));
  if (!isMe && savedVolumes()[peer.name] === 0) {
    side.append(statusIcon(icon("volume-off"), "Вы заглушили его для себя"));
  }
  if (!isMe) {
    // Громкость и права хоста — в меню человека, чтобы список был чистым.
    const menu = peerMenu(peer);
    side.append(menu.wrap);
    li.classList.add("has-menu");
    li.addEventListener("click", (event) => {
      if (event.target.closest("button, .menu")) return;
      event.stopPropagation();
      menu.toggle();
    });
  }

  // Телефон: перевернуть свою камеру — прямо на своей плитке, как в Telegram.
  const flip = camera && isMe && isAndroid ? cameraFlipButton() : null;
  if ($("peers").classList.contains("tiles")) {
    tileLayout(li, peer, avatar, main, side);
    if (camera) li.prepend(camera.box);
    if (flip) li.append(flip);
  } else {
    if (camera) avatar.append(camera.box);
    if (flip) side.prepend(flip);
    li.append(avatar, main, side);
  }
  return li;
}

function cameraFlipButton() {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "cam-flip";
  button.setAttribute("aria-label", "Перевернуть камеру");
  button.title = call.cameraId === "back" ? "Включить фронтальную камеру" : "Включить основную камеру";
  button.append(icon("camera-flip"));
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    const next = call.cameraId === "back" ? "front" : "back";
    store.set("camera", next);
    startCamera(next);
  });
  return button;
}

// Плитка «сцена»: фон цвета аватара, вверху — «в эфире» и связь, в центре —
// аватар, внизу — плашка с именем и значками, справа от неё «Смотреть».
function tileLayout(li, peer, avatar, main, side) {
  li.style.setProperty("--tile", avatarColor(peer.name));
  const box = (className, ...children) => {
    const div = document.createElement("div");
    div.className = className;
    div.append(...children.filter(Boolean));
    return div;
  };
  const icons = side.querySelectorAll(".status-icon:not(.quality)");
  const parts = [
    box("tile-top", main.querySelector(".badge.live"), side.querySelector(".quality")),
    box("tile-center", avatar, main.querySelector(".badge.off")),
    box(
      "tile-bottom",
      box("tile-name", main.querySelector(".peer-name"), ...icons),
      side.querySelector(".watch-btn"),
    ),
    side.querySelector(".menu-wrap"),
  ];
  li.append(...parts.filter(Boolean));
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

// Меню человека: громкость, «заглушить для себя», а хосту — заглушить для
// всех и выгнать. Открывается кнопкой «⋮» или нажатием на строку.
function peerMenu(peer) {
  const wrap = document.createElement("div");
  wrap.className = "menu-wrap";
  const menu = document.createElement("div");
  menu.className = "menu peer-menu";
  menu.hidden = true;
  // Нажатия внутри меню (ползунок, «Точно выгнать?») не должны его закрывать.
  menu.addEventListener("click", (event) => event.stopPropagation());

  const toggle = () => {
    const open = !menu.hidden;
    closeMenus();
    if (open) return;
    // У нижних строк меню открывается вверх, иначе его обрезал бы край списка.
    const list = $("peers").getBoundingClientRect();
    const row = wrap.getBoundingClientRect();
    menu.classList.toggle("up", row.top > list.top + list.height / 2);
    menu.classList.add("right");
    // Качество связи — свежее на момент открытия.
    menu.querySelector(".quality-note")?.remove();
    const quality = call.quality.get(peer.id);
    if (quality) title.after(qualityLine(quality));
    menu.style.right = "";
    menu.hidden = false;
    // У плитки в левом ряду меню шире самой плитки — сдвигаем, чтобы не
    // уходило за левый край окна.
    const left = menu.getBoundingClientRect().left;
    if (left < 8) menu.style.right = `${left - 8}px`;
  };
  const button = toggleButton(null, icon("more"), false, "", (event) => {
    event.stopPropagation();
    toggle();
  });
  // Без всплывающей подсказки: она закрывала бы открытое меню.
  button.removeAttribute("title");
  button.setAttribute("aria-label", `Действия: ${peer.name}`);

  const title = document.createElement("div");
  title.className = "menu-title";
  title.textContent = peer.name;

  const volume = volumeControl(peer.id, peer.name);

  const localMute = document.createElement("button");
  localMute.type = "button";
  const mutedForMe = savedVolumes()[peer.name] === 0;
  localMute.append(icon("volume-off"));
  localMute.append(mutedForMe ? "Снова слышать" : "Заглушить для себя");
  localMute.addEventListener("click", () => {
    closeMenus();
    setLocalMute(peer, !mutedForMe);
  });

  // На телефоне подсказку значка не увидеть — качество связи пишем и в меню.
  menu.append(title, volume, localMute);

  if (call.me.admin) {
    const mute = document.createElement("button");
    mute.type = "button";
    mute.append(icon("mic", !peer.serverMuted));
    mute.append(peer.serverMuted ? "Вернуть голос" : "Заглушить для всех");
    mute.addEventListener("click", () => {
      closeMenus();
      invoke("set_server_mute", { id: peer.id, muted: !peer.serverMuted });
    });

    // Выгнать — нужно подтвердить вторым нажатием.
    const kick = document.createElement("button");
    kick.type = "button";
    kick.className = "danger-item";
    kick.textContent = "Выгнать из комнаты";
    kick.addEventListener("click", () => {
      if (!kick.classList.contains("armed")) {
        kick.classList.add("armed");
        kick.textContent = "Точно выгнать?";
        return;
      }
      closeMenus();
      invoke("kick", { id: peer.id });
    });

    const separator = document.createElement("div");
    separator.className = "menu-sep";
    menu.append(separator, mute, kick);
  }

  wrap.append(button, menu);
  return { wrap, toggle };
}

function qualityLine(quality) {
  const line = document.createElement("div");
  line.className = `menu-note quality-note ${quality.level}`;
  line.textContent = quality.text;
  return line;
}

// «Заглушить для себя» — громкость 0; прежняя громкость возвращается обратно.
function setLocalMute(peer, muted) {
  const volumes = savedVolumes();
  const before = store.get("volumesBeforeMute", {});
  let percent;
  if (muted) {
    before[peer.name] = volumes[peer.name] || 100;
    percent = 0;
  } else {
    percent = before[peer.name] || 100;
    delete before[peer.name];
  }
  volumes[peer.name] = percent;
  store.set("volumes", volumes);
  store.set("volumesBeforeMute", before);
  invoke("set_volume", { id: peer.id, volume: percent / 100 }).catch(() => {});
  renderPeers();
}

function closeMenus() {
  for (const menu of document.querySelectorAll(".menu")) menu.hidden = true;
  $("address-btn").setAttribute("aria-expanded", "false");
  $("leave-btn").setAttribute("aria-expanded", "false");
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
  const row = document.createElement("label");
  row.className = "peer-volume";
  const label = document.createElement("span");
  label.textContent = "Громкость";
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
  row.append(label, slider, value);
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
  checkNewerApp();
});

listen("peer-left", ({ payload }) => {
  call.peers.delete(payload.id);
  avatars.delete(payload.id);
  call.quality.delete(payload.id);
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

// Микрофон или звук переключили из меню в трее.
listen("local-status", ({ payload }) => {
  if (!call.me) return;
  if (call.me.muted === payload.muted && call.me.deafened === payload.deafened) return;
  call.me.muted = payload.muted;
  call.me.deafened = payload.deafened;
  renderPeers();
});

listen("call-ended", async ({ payload }) => {
  await invoke("disconnect").catch(() => {});
  if (call.hosting) await invoke("stop_server").catch(() => {});
  leaveRoom(`Звонок завершён: ${payload.reason}`);
});

// ---------------------------------------------------------------------------
// Вкладки и чат
// ---------------------------------------------------------------------------

let currentTab = "people";

function showTab(tab) {
  currentTab = tab;
  store.set("roomTab", tab);
  $("peers").hidden = tab !== "people";
  $("chat").hidden = tab !== "chat";
  $("peer-layout").hidden = tab !== "people";
  if (tab === "chat") {
    setUnread(0);
    renderChat(true);
    $("chat-input").focus();
  }
}

const selectTab = segmented($("room-tabs"), "people", showTab);

// Участники списком или плитками; выбор запоминается.
function setPeerLayout(layout) {
  store.set("peerLayout", layout);
  $("peers").classList.toggle("tiles", layout === "tiles");
  // Плитка и строка устроены по-разному — собираем участников заново.
  if (call.me) renderPeers();
}

segmented($("peer-layout"), store.get("peerLayout", "list"), setPeerLayout);
setPeerLayout(store.get("peerLayout", "list"));

function setUnread(count) {
  call.unread = count;
  $("chat-unread").hidden = count === 0;
  $("chat-unread").textContent = count > 99 ? "99+" : String(count);
}

// Добавляет сообщение в список (без повторов). true — если оно новое.
function addMessage(message) {
  if (call.chat.some((m) => m.id === message.id)) return false;
  call.chat.push(message);
  call.chat.sort((a, b) => a.id - b.id);
  if (call.chat.length > 500) call.chat.shift();
  return true;
}

listen("chat-message", ({ payload }) => {
  if (!call.me) {
    call.chatPending.push(payload);
    return;
  }
  if (!addMessage(payload)) return;
  const mine = payload.from === call.me.id;
  if (!mine && (currentTab !== "chat" || document.hidden)) setUnread(call.unread + 1);
  // Android сам решает, показывать ли уведомление: только если программа свёрнута.
  if (!mine) androidBridge?.chatMessage(call.title, payload.name, payload.text, payload.timeMs);
  renderChat(mine);
});

// Android: нажали уведомление о сообщении.
window.onAndroidOpenChat = () => {
  if (!call.me) return;
  closeScreenView();
  selectTab("chat");
  showTab("chat");
};

// Прочитали, вернувшись в окно с открытым чатом.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && currentTab === "chat") setUnread(0);
});

// Сообщения одного автора подряд (с паузой меньше 5 минут) — одной группой.
const GROUP_GAP_MS = 5 * 60 * 1000;

// forceBottom — прокрутить вниз в любом случае (своё сообщение, открытие
// вкладки); иначе — только если пользователь и так был внизу.
function renderChat(forceBottom) {
  const log = $("chat-log");
  if ($("chat").hidden) return;
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
  log.innerHTML = "";
  if (call.chat.length === 0) {
    const empty = document.createElement("div");
    empty.className = "chat-empty";
    empty.textContent = "Сообщений пока нет. Напишите первым!";
    log.append(empty);
    return;
  }
  let body = null;
  let last = null;
  for (const message of call.chat) {
    const continues =
      last && last.from === message.from && last.name === message.name &&
      message.timeMs - last.timeMs < GROUP_GAP_MS;
    if (!continues) {
      const group = document.createElement("div");
      group.className = "msg-group";
      const avatar = document.createElement("div");
      avatar.className = "avatar";
      paintAvatar(avatar, message.name, avatarOf(message.from));
      body = document.createElement("div");
      body.className = "msg-body";
      const head = document.createElement("div");
      head.className = "msg-head";
      const name = document.createElement("span");
      name.className = "msg-name";
      name.textContent = message.name;
      const time = document.createElement("span");
      time.className = "msg-time";
      time.textContent = formatTime(message.timeMs);
      head.append(name, time);
      body.append(head);
      group.append(avatar, body);
      log.append(group);
    }
    const text = document.createElement("div");
    text.className = "msg-text";
    text.title = formatTime(message.timeMs, true);
    text.append(...linkify(message.text));
    body.append(text);
    last = message;
  }
  if (forceBottom || atBottom) log.scrollTop = log.scrollHeight;
}

// «14:05», для вчерашних и старше — «3 окт., 14:05».
function formatTime(ms, full = false) {
  const date = new Date(ms);
  const time = date.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
  if (!full && date.toDateString() === new Date().toDateString()) return time;
  return `${date.toLocaleDateString("ru-RU", { day: "numeric", month: "short" })}, ${time}`;
}

// Текст с кликабельными ссылками. Только узлы DOM, никакого innerHTML:
// текст прислал другой человек.
const URL_RE = /https?:\/\/[^\s<>"']+[^\s<>"'.,;:!?)\]}]/gi;

function linkify(text) {
  const nodes = [];
  let pos = 0;
  for (const match of text.matchAll(URL_RE)) {
    if (match.index > pos) nodes.push(document.createTextNode(text.slice(pos, match.index)));
    const link = document.createElement("a");
    link.textContent = match[0];
    link.title = "Открыть в браузере";
    link.addEventListener("click", (event) => {
      event.preventDefault();
      invoke("open_url", { url: match[0] }).catch((e) => toast(errorText(e)));
    });
    nodes.push(link);
    pos = match.index + match[0].length;
  }
  if (pos < text.length) nodes.push(document.createTextNode(text.slice(pos)));
  return nodes;
}

// Поле ввода растёт вместе с текстом (до 5 строк).
function fitInput() {
  const input = $("chat-input");
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 120)}px`;
}

$("chat-input").addEventListener("input", fitInput);

$("chat-input").addEventListener("keydown", (event) => {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    $("chat-form").requestSubmit();
  }
});

$("chat-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = $("chat-input");
  const text = input.value.trim();
  if (!text) return;
  try {
    await invoke("send_chat", { text });
    input.value = "";
    fitInput();
  } catch (e) {
    toast(`Не отправилось: ${errorText(e)}`);
  }
});

// ---------------------------------------------------------------------------
// Демонстрация экрана
// ---------------------------------------------------------------------------

// «Смотреть» — окно с экраном участника. Свой экран тоже можно открыть:
// так видно, что получают остальные.
function watchButton(peer, isMe) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "watch-btn";
  button.title = isMe ? "Посмотреть, что видят остальные" : `Смотреть экран: ${peer.name}`;
  button.append(icon("screen"));
  if (!isMe) {
    const label = document.createElement("span");
    label.className = "watch-label";
    label.textContent = "Смотреть";
    button.append(label);
  }
  button.addEventListener("click", () => {
    if (isAndroid) return openScreenView(peer);
    invoke("open_viewer", { id: peer.id, name: peer.name }).catch((e) => toast(errorText(e)));
  });
  return button;
}

// Телефон: чужой экран — слоем на весь экран поверх комнаты. Пальцами можно
// увеличить и подвинуть картинку, двойное касание — увеличить или вернуть.
const sv = {
  view: null,
  peer: null,
  kind: null, // "screen" или "camera"
  scale: 1,
  x: 0,
  y: 0,
  pointers: new Map(),
  pinch: null,
  landscape: false,
  lastTap: 0,
  barTimer: null,
};

async function openScreenView(peer) {
  closeScreenView();
  try {
    await invoke("open_viewer", { id: peer.id, name: peer.name });
  } catch (e) {
    return toast(errorText(e));
  }
  sv.peer = peer.id;
  sv.kind = "screen";
  $("sv-title").textContent = `Экран: ${peer.name}`;
  showOverlay(peer.name);
  sv.view = createScreenView({
    peer: peer.id,
    canvas: $("sv-canvas"),
    img: $("sv-img"),
    onFrame: () => ($("sv-waiting").hidden = true),
  });
}

function showOverlay(name) {
  $("sv-waiting-text").textContent = `Жду картинку от ${name}…`;
  $("sv-waiting").hidden = false;
  resetZoom();
  showBar();
  $("screen-view").hidden = false;
  androidBridge?.setImmersive(true);
}

// Камера на весь экран — самый большой слой. Плитка этого участника на
// время отдаёт ему свой поток (у участника один поток камеры на окно).
function openCameraView(id) {
  const isMe = id === call.me?.id;
  const peer = isMe ? call.me : call.peers.get(id);
  if (!peer) return;
  closeScreenView();
  sv.peer = id;
  sv.kind = "camera";
  syncCameraTile(id);
  $("sv-title").textContent = isMe ? "Ваша камера" : `Камера: ${peer.name}`;
  $("sv-canvas").classList.toggle("mirror", isMe && call.cameraId !== "back");
  showOverlay(peer.name);
  sv.view = createScreenView({
    peer: id,
    canvas: $("sv-canvas"),
    img: null,
    onFrame: () => ($("sv-waiting").hidden = true),
    onFail: () => {
      closeScreenView();
      toast("Это окно не умеет показывать видео камеры");
    },
    camera: { layer: isMe ? 0 : CAMERA_TOP_LAYER },
  });
}

function closeScreenView() {
  if (!sv.view) return false;
  const { peer, kind } = sv;
  sv.view.stop();
  if (kind === "screen") invoke("stop_watching", { id: peer }).catch(() => {});
  sv.view = null;
  sv.peer = null;
  sv.kind = null;
  $("sv-canvas").classList.remove("mirror");
  $("screen-view").hidden = true;
  androidBridge?.setImmersive(false);
  if (sv.landscape) setLandscape(false);
  if (kind === "camera") syncCameraTile(peer);
  return true;
}

function setLandscape(on) {
  sv.landscape = on;
  $("sv-rotate").classList.toggle("active", on);
  androidBridge?.setLandscape(on);
}

// Панель сверху прячется через несколько секунд; касание показывает снова.
function showBar() {
  $("sv-bar").classList.remove("hidden");
  clearTimeout(sv.barTimer);
  sv.barTimer = setTimeout(() => {
    if ($("sv-waiting").hidden) $("sv-bar").classList.add("hidden");
  }, 3000);
}

function toggleBar() {
  if ($("sv-bar").classList.contains("hidden")) showBar();
  else $("sv-bar").classList.add("hidden");
}

function resetZoom() {
  sv.scale = 1;
  sv.x = 0;
  sv.y = 0;
  applyZoom();
}

function applyZoom() {
  const rect = $("sv-stage").getBoundingClientRect();
  const maxX = (rect.width * (sv.scale - 1)) / 2;
  const maxY = (rect.height * (sv.scale - 1)) / 2;
  sv.x = Math.max(-maxX, Math.min(maxX, sv.x));
  sv.y = Math.max(-maxY, Math.min(maxY, sv.y));
  $("sv-content").style.transform = `translate(${sv.x}px, ${sv.y}px) scale(${sv.scale})`;
}

// Точка касания относительно центра картинки.
function fromCenter(x, y) {
  const rect = $("sv-stage").getBoundingClientRect();
  return { x: x - rect.left - rect.width / 2, y: y - rect.top - rect.height / 2 };
}

// Увеличить до `scale`, не сдвигая точку `at` (относительно центра).
function zoomAt(scale, at, from = { scale: sv.scale, x: sv.x, y: sv.y }) {
  const k = scale / from.scale;
  sv.scale = scale;
  sv.x = at.x - (at.x - from.x) * k;
  sv.y = at.y - (at.y - from.y) * k;
  applyZoom();
}

function pinchState() {
  const [a, b] = [...sv.pointers.values()];
  return {
    dist: Math.hypot(a.x - b.x, a.y - b.y) || 1,
    mid: fromCenter((a.x + b.x) / 2, (a.y + b.y) / 2),
  };
}

const stage = $("sv-stage");
stage.addEventListener("pointerdown", (event) => {
  stage.setPointerCapture(event.pointerId);
  sv.pointers.set(event.pointerId, {
    x: event.clientX,
    y: event.clientY,
    startX: event.clientX,
    startY: event.clientY,
    time: performance.now(),
  });
  if (sv.pointers.size === 2) {
    const { dist, mid } = pinchState();
    sv.pinch = { dist, mid, scale: sv.scale, x: sv.x, y: sv.y };
  }
});

stage.addEventListener("pointermove", (event) => {
  const pointer = sv.pointers.get(event.pointerId);
  if (!pointer) return;
  const dx = event.clientX - pointer.x;
  const dy = event.clientY - pointer.y;
  pointer.x = event.clientX;
  pointer.y = event.clientY;
  if (sv.pinch && sv.pointers.size === 2) {
    const { dist, mid } = pinchState();
    const scale = Math.max(1, Math.min(5, (sv.pinch.scale * dist) / sv.pinch.dist));
    // Двигаем вместе с пальцами: точка под серединой пальцев остаётся под ними.
    const from = { scale: sv.pinch.scale, x: sv.pinch.x, y: sv.pinch.y };
    zoomAt(scale, sv.pinch.mid, from);
    sv.x += mid.x - sv.pinch.mid.x;
    sv.y += mid.y - sv.pinch.mid.y;
    applyZoom();
  } else if (sv.pointers.size === 1 && sv.scale > 1) {
    sv.x += dx;
    sv.y += dy;
    applyZoom();
  }
});

function pointerEnd(event) {
  const pointer = sv.pointers.get(event.pointerId);
  if (!pointer) return;
  sv.pointers.delete(event.pointerId);
  if (sv.pinch) {
    if (sv.pointers.size < 2) sv.pinch = null;
    return;
  }
  const moved = Math.hypot(event.clientX - pointer.startX, event.clientY - pointer.startY);
  if (event.type === "pointercancel" || moved > 12 || performance.now() - pointer.time > 300) {
    return;
  }
  const now = performance.now();
  if (now - sv.lastTap < 300) {
    sv.lastTap = 0;
    if (sv.scale > 1) resetZoom();
    else zoomAt(2.5, fromCenter(event.clientX, event.clientY));
  } else {
    sv.lastTap = now;
    toggleBar();
  }
}

stage.addEventListener("pointerup", pointerEnd);
stage.addEventListener("pointercancel", pointerEnd);
$("sv-close").addEventListener("click", closeScreenView);
$("sv-rotate").addEventListener("click", () => {
  setLandscape(!sv.landscape);
  showBar();
});
// Повернули телефон — картинка другого размера, сдвиг пересчитываем.
window.addEventListener("resize", () => sv.view && applyZoom());

// «Назад» на телефоне (MainActivity.kt): закрывает просмотр экрана; если
// закрывать нечего — программа сворачивается.
window.onAndroidBack = () => closeScreenView();

// Android: кнопка микрофона в уведомлении звонка.
window.onAndroidToggleMic = () => {
  if (call.me) setMuted(!call.me.muted);
};

// Android: «Выйти» в уведомлении звонка — как кнопка «Выйти» (хосту — с вопросом).
window.onAndroidLeave = () => {
  if (call.me) $("leave-btn").click();
};

function setSharing(on) {
  call.sharing = on;
  if (!on) call.viewers = 0;
  renderDock();
}

let shareSource = null;
const shareQuality = { height: store.get("shareHeight", "1080"), fps: store.get("shareFps", "30") };
segmented($("share-quality"), shareQuality.height, (v) => {
  shareQuality.height = v;
  store.set("shareHeight", v);
});
segmented($("share-fps"), shareQuality.fps, (v) => {
  shareQuality.fps = v;
  store.set("shareFps", v);
});

$("share-btn").addEventListener("click", async () => {
  if (call.sharing) {
    await invoke("stop_share").catch(() => {});
    setSharing(false);
    return;
  }
  openShareSheet();
});

async function openShareSheet() {
  closeMenus();
  $("share-sheet").hidden = false;
  const list = $("share-sources");
  list.innerHTML = "";
  $("share-start").disabled = true;
  const loading = document.createElement("li");
  loading.className = "hint";
  loading.textContent = "Ищу экраны и окна…";
  list.append(loading);
  let sources;
  try {
    sources = await invoke("screen_sources");
  } catch (e) {
    loading.textContent = `Не удалось: ${errorText(e)}`;
    return;
  }
  list.innerHTML = "";
  const saved = store.get("shareSource", null);
  shareSource = sources.some((s) => s.id === saved) ? saved : sources[0]?.id ?? null;
  let lastKind = null;
  for (const source of sources) {
    if (source.window !== lastKind && sources.some((s) => s.window)) {
      const head = document.createElement("li");
      head.className = "section";
      head.textContent = source.window ? "Окна" : "Экраны";
      list.append(head);
      lastKind = source.window;
    }
    const li = document.createElement("li");
    const button = document.createElement("button");
    button.type = "button";
    button.className = "source";
    button.classList.toggle("selected", source.id === shareSource);
    button.title = source.name;
    const label = document.createElement("span");
    label.className = "label";
    label.textContent = source.name;
    button.append(icon(source.window ? "window" : "screen"), label);
    button.addEventListener("click", () => {
      shareSource = source.id;
      for (const other of list.querySelectorAll(".source")) other.classList.remove("selected");
      button.classList.add("selected");
      $("share-start").disabled = false;
    });
    button.addEventListener("dblclick", () => $("share-start").click());
    li.append(button);
    list.append(li);
  }
  if (sources.length === 0) {
    const empty = document.createElement("li");
    empty.className = "hint";
    empty.textContent = "Нечего показать.";
    list.append(empty);
  }
  $("share-start").disabled = !shareSource;
}

function closeShareSheet() {
  $("share-sheet").hidden = true;
}

$("share-close").addEventListener("click", closeShareSheet);
$("share-sheet").addEventListener("click", (event) => {
  if (event.target === $("share-sheet")) closeShareSheet();
});

$("share-start").addEventListener("click", async () => {
  if (!shareSource) return;
  const button = $("share-start");
  button.disabled = true;
  button.textContent = "Запускаю…";
  try {
    await invoke("start_share", {
      source: shareSource,
      maxHeight: Number(shareQuality.height),
      fps: Number(shareQuality.fps),
    });
    store.set("shareSource", shareSource);
    setSharing(true);
    closeShareSheet();
  } catch (e) {
    toast(errorText(e));
  } finally {
    button.disabled = false;
    button.textContent = "Начать показ";
  }
});

listen("share-viewers", ({ payload }) => {
  call.viewers = payload.count;
  if (call.sharing) setSharing(true);
  if (call.me) renderPeers();
});

listen("share-stopped", ({ payload }) => {
  setSharing(false);
  invoke("stop_share").catch(() => {});
  toast(`Показ экрана остановлен: ${payload.reason}`);
});

// ---------------------------------------------------------------------------
// Камера (src/camera.rs)
// ---------------------------------------------------------------------------

// Слои качества, как в групповых звонках Telegram: 0 — маленький поток для
// плиток, 1 — для крупной плитки, 2 — на весь экран. Плитка просит слой по
// своему размеру в точках экрана, а сервер сам опускает его, если сеть не
// тянет, и поднимает обратно.
const CAMERA_TOP_LAYER = 2;
const CAMERA_LAYER_1_FROM = 300; // высота плитки в точках экрана для слоя 1

// Видео в плитках: id → { box, canvas, view, layer, visible, failed, isMe }.
// Переживает перерисовку списка (renderPeers строит его заново): холст с
// декодером просто переезжает в новую плитку, и картинка не мигает.
const cameraTiles = new Map();

// Смотрим только то, что видно: плитка ушла за край — отписываемся (не
// сразу: при прокрутке туда-обратно поток не дёргается).
const cameraVisibility = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    const id = Number(entry.target.dataset.peer);
    const tile = cameraTiles.get(id);
    if (!tile) continue;
    clearTimeout(tile.hideTimer);
    if (entry.isIntersecting) {
      tile.visible = true;
      syncCameraTile(id);
    } else {
      tile.hideTimer = setTimeout(() => {
        tile.visible = false;
        syncCameraTile(id);
      }, 2000);
    }
  }
});

// Плитка стала больше или меньше — другой слой.
const cameraSize = new ResizeObserver((entries) => {
  for (const entry of entries) {
    const id = Number(entry.target.dataset.peer);
    const tile = cameraTiles.get(id);
    if (!tile || tile.isMe) continue;
    const height = entry.contentRect.height * (window.devicePixelRatio || 1);
    tile.layer = height >= CAMERA_LAYER_1_FROM ? 1 : 0;
    tile.view?.setLayer(tile.layer);
  }
});

function cameraTile(id, isMe) {
  let tile = cameraTiles.get(id);
  if (tile) return tile;
  const box = document.createElement("div");
  box.className = "cam-video";
  box.dataset.peer = id;
  box.title = "Открыть на весь экран";
  const canvas = document.createElement("canvas");
  canvas.hidden = true;
  box.append(canvas);
  box.addEventListener("click", (event) => {
    event.stopPropagation();
    openCameraView(id);
  });
  tile = { box, canvas, view: null, layer: 0, visible: false, failed: false, isMe, hideTimer: null };
  cameraTiles.set(id, tile);
  cameraVisibility.observe(box);
  cameraSize.observe(box);
  return tile;
}

// Показывать ли сейчас видео в плитке — и запустить или остановить его.
function syncCameraTile(id) {
  const tile = cameraTiles.get(id);
  if (!tile) return;
  const want =
    tile.visible && !document.hidden && !tile.failed && !(sv.kind === "camera" && sv.peer === id);
  if (want && !tile.view) {
    tile.view = createScreenView({
      peer: id,
      canvas: tile.canvas,
      img: null,
      onFrame: () => tile.box.classList.add("playing"),
      onFail: () => {
        tile.failed = true;
        syncCameraTile(id);
      },
      camera: { layer: tile.layer },
    });
  } else if (!want && tile.view) {
    tile.view.stop();
    tile.view = null;
    tile.box.classList.remove("playing");
  }
}

// После перерисовки: убрать плитки тех, у кого камера выключилась или кто ушёл.
function pruneCameraTiles() {
  for (const [id, tile] of cameraTiles) {
    const on = id === call.me?.id ? call.camera : call.peers.get(id)?.camera;
    if (on) continue;
    clearTimeout(tile.hideTimer);
    tile.view?.stop();
    cameraVisibility.unobserve(tile.box);
    cameraSize.unobserve(tile.box);
    tile.box.remove();
    cameraTiles.delete(id);
  }
}

function clearCameraTiles() {
  for (const [, tile] of cameraTiles) {
    clearTimeout(tile.hideTimer);
    tile.view?.stop();
    cameraVisibility.unobserve(tile.box);
    cameraSize.unobserve(tile.box);
    tile.box.remove();
  }
  cameraTiles.clear();
}

// Свернули окно (или телефон убрал программу с экрана) — чужие камеры не
// нужны, трафик не тратим. Своя камера при этом продолжает идти.
document.addEventListener("visibilitychange", () => {
  for (const id of cameraTiles.keys()) syncCameraTile(id);
});

// Android: разрешение на камеру спрашивает Kotlin, ответ — сюда.
let cameraPermission = null;
window.onAndroidCamera = (granted) => {
  cameraPermission?.(Boolean(granted));
  cameraPermission = null;
};

function askCameraPermission() {
  if (!androidBridge) return Promise.resolve(true);
  return new Promise((resolve) => {
    cameraPermission = resolve;
    androidBridge.requestCameraPermission();
  });
}

async function startCamera(id = store.get("camera", "")) {
  if (isAndroid) {
    if (!(await askCameraPermission())) {
      toast("Без разрешения на камеру видео не включить. Его можно дать в настройках Android");
      return;
    }
    if (id !== "back") id = "front";
  }
  try {
    await invoke("start_camera", { id });
  } catch (e) {
    toast(capitalized(errorText(e)));
    return;
  }
  // Rust уже ждёт кадры — теперь их можно снимать (на телефоне снимает Kotlin).
  androidBridge?.startCamera(id !== "back");
  call.camera = true;
  call.cameraId = id;
  call.cameraViewers = 0;
  renderPeers();
}

async function stopCamera() {
  androidBridge?.stopCamera();
  call.camera = false;
  call.cameraViewers = 0;
  renderPeers();
  await invoke("stop_camera").catch(() => {});
}

$("camera-btn").addEventListener("click", () => {
  if (call.camera) stopCamera();
  else startCamera();
});

// Какую камеру включать: список под стрелкой у кнопки камеры.
async function openCameraMenu() {
  const menu = $("camera-menu");
  const wasOpen = !menu.hidden;
  closeMenus();
  if (wasOpen) return;
  let list;
  try {
    list = await invoke("camera_sources");
  } catch (e) {
    return toast(capitalized(errorText(e)));
  }
  menu.innerHTML = "";
  const title = document.createElement("div");
  title.className = "menu-title";
  title.textContent = "Камера";
  menu.append(title);
  if (!list.length) {
    const empty = document.createElement("div");
    empty.className = "menu-empty";
    empty.textContent = "Камер не найдено";
    menu.append(empty);
  }
  const current = call.camera ? call.cameraId : store.get("camera", "");
  const currentId = list.some((c) => c.id === current) ? current : list[0]?.id;
  for (const camera of list) {
    const item = document.createElement("button");
    item.type = "button";
    item.classList.toggle("current", camera.id === currentId);
    const check = icon("check");
    check.classList.add("check");
    const text = document.createElement("span");
    text.className = "label";
    text.textContent = camera.name;
    item.append(check, text);
    item.addEventListener("click", () => {
      closeMenus();
      store.set("camera", camera.id);
      // Включена — переключаем на выбранную, выключена — включаем её.
      startCamera(camera.id);
    });
    menu.append(item);
  }
  menu.hidden = false;
}

// Стрелка выбора камеры — только когда выбирать есть из чего (на телефоне
// вместо неё кнопка «перевернуть» на своей плитке). Так внизу просторнее.
async function refreshCameraPill() {
  if (isAndroid || !call.me) return;
  try {
    const list = await invoke("camera_sources");
    $("camera-pill").hidden = list.length < 2;
  } catch {
    $("camera-pill").hidden = true;
  }
}

window.addEventListener("focus", refreshCameraPill);

$("camera-pill").addEventListener("click", (event) => {
  event.stopPropagation();
  openCameraMenu();
});
$("camera-menu").addEventListener("click", (event) => event.stopPropagation());

listen("camera-viewers", ({ payload }) => {
  // Зритель получает один слой (лишь на мгновение переключения — два),
  // поэтому сумма по слоям — это число зрителей.
  call.cameraViewers = payload.layers.reduce((a, b) => a + b, 0);
  renderDock();
});

listen("camera-stopped", ({ payload }) => {
  androidBridge?.stopCamera();
  call.camera = false;
  renderPeers();
  toast(`Камера отключилась: ${payload.reason}`);
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
  $("room-view").querySelector(".dock").classList.toggle("speaking", status.transmitting);
  checkTalkingMuted(status.levelDb);
}

// Говорят с выключенным микрофоном — подсказываем включить. Около секунды
// голоса громче порога подряд (короткий кашель или стук не в счёт), и не
// чаще раза в 20 секунд.
const mutedTalk = { score: 0, quietUntil: 0, timer: null };

function checkTalkingMuted(levelDb) {
  const me = call.me;
  if (!me?.muted || me.deafened || me.serverMuted) {
    mutedTalk.score = 0;
    hideMutedHint();
    return;
  }
  const threshold = Math.max(store.get("vad", -45), -50);
  mutedTalk.score = levelDb >= threshold ? mutedTalk.score + 1 : Math.max(0, mutedTalk.score - 0.34);
  if (mutedTalk.score < 8 || Date.now() < mutedTalk.quietUntil) return;
  mutedTalk.score = 0;
  mutedTalk.quietUntil = Date.now() + 20_000;
  $("muted-hint").hidden = false;
  clearTimeout(mutedTalk.timer);
  mutedTalk.timer = setTimeout(hideMutedHint, 6000);
}

function hideMutedHint() {
  clearTimeout(mutedTalk.timer);
  $("muted-hint").hidden = true;
}

// Качество связи с каждым собеседником — по счётчикам звука за последние
// 2 секунды: сколько пакетов потерялось или опоздало и какая задержка.
const qualityCounters = new Map(); // id -> { received, lost, late }

const QUALITY_TEXT = { good: "Связь хорошая", fair: "Связь так себе", bad: "Связь плохая" };

async function refreshQuality() {
  let stats;
  try {
    stats = await invoke("audio_stats");
  } catch {
    return;
  }
  if (!stats || !call.me) return;
  for (const peer of stats.peers) {
    const prev = qualityCounters.get(peer.id) ?? { received: 0, lost: 0, late: 0 };
    qualityCounters.set(peer.id, { received: peer.received, lost: peer.lost, late: peer.late });
    const received = peer.received - prev.received;
    // Человек молчит — звука нет, и судить не по чему: оставляем прежнее.
    if (received < 10) continue;
    const lostPct = (100 * Math.max(0, peer.lost - prev.lost)) / received;
    const latePct = (100 * Math.max(0, peer.late - prev.late)) / received;
    const delay = Math.round(peer.delayMs);
    let level = "good";
    if (lostPct > 5 || latePct > 5 || delay > 300) level = "bad";
    else if (lostPct > 1.5 || latePct > 2 || delay > 150) level = "fair";
    const text = `${QUALITY_TEXT[level]}: задержка ${delay} мс, потери ${lostPct.toFixed(1).replace(".", ",")}%`;
    call.quality.set(peer.id, { level, text });
  }
  for (const li of document.querySelectorAll(".peer:not(.me)")) {
    li.querySelector(".quality")?.replaceWith(qualityIcon(Number(li.dataset.id)));
  }
}

// Значок качества связи; пока данных нет — пустое место той же ширины.
function qualityIcon(id) {
  const q = call.quality.get(id);
  const span = document.createElement("span");
  span.className = "status-icon quality";
  if (!q) {
    span.hidden = true;
    return span;
  }
  span.classList.add(q.level);
  span.title = q.text;
  span.setAttribute("aria-label", q.text);
  // Сколько палочек — как у сигнала телефона: не только цветом.
  const back = icon("signal");
  back.classList.add("signal-back");
  span.append(back, icon(q.level === "good" ? "signal" : `signal-${q.level}`));
  return span;
}

$("muted-hint").addEventListener("click", () => {
  hideMutedHint();
  if (call.me?.muted) setMuted(false);
});


// ---------------------------------------------------------------------------
// Управление передачей
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// Доступ через интернет (пробивка NAT)
// ---------------------------------------------------------------------------

let internetTimer = null;

async function setInternet(enabled) {
  try {
    renderInternet(await invoke("set_internet", { enabled }));
  } catch (e) {
    $("internet").checked = false;
    toast(errorText(e));
    renderInternet(null);
    return;
  }
  stopInternetPolling();
  if (enabled) internetTimer = setInterval(refreshInternet, 2000);
}

function stopInternetPolling() {
  clearInterval(internetTimer);
  internetTimer = null;
}

async function refreshInternet() {
  try {
    renderInternet(await invoke("internet_status"));
  } catch {
    // Комнату закрыли — опрос остановит leaveRoom.
  }
}

function renderInternet(status) {
  const info = $("internet-info");
  info.hidden = !status?.enabled;
  if (!status?.enabled) return;
  $("invite-code").textContent = status.invite;
  const state = $("internet-state");
  state.classList.remove("warning-text");
  if (status.error) {
    state.textContent = `Не получается: ${status.error}`;
    state.classList.add("warning-text");
  } else if (!status.public) {
    state.textContent = "Определяю внешний адрес…";
  } else if (status.symmetric && !status.mapped) {
    state.textContent =
      `Внешний адрес ${status.public}, но ваш роутер выдаёт разным собеседникам разные порты ` +
      "(«симметричный» NAT). Друзья, скорее всего, не подключатся — нужен Tailscale.";
    state.classList.add("warning-text");
  } else {
    const guests = status.guests ? ` · подключений по коду: ${status.guests}` : "";
    const mapped = status.mapped ? ` · роутер открыл порт ${status.mapped}` : "";
    state.textContent = `Готово. Внешний адрес ${status.public}${mapped}${guests}`;
  }
}

$("internet").addEventListener("change", () => setInternet($("internet").checked));

$("room-lan").addEventListener("change", async () => {
  if (!call.hostRoom) return;
  const visible = $("room-lan").checked;
  try {
    await invoke("set_room_lan", { id: call.hostRoom.id, visible });
    call.hostRoom.lan = visible;
  } catch (e) {
    $("room-lan").checked = !visible;
    toast(errorText(e));
  }
});

$("invite-copy").addEventListener("click", () => copyText($("invite-code").textContent));

// Текст приглашения для мессенджера. Пароль — в нём же, чтобы друг вошёл
// одним нажатием (см. joinFromInvite).
function inviteMessage() {
  const room = call.hostRoom?.name ?? call.title;
  const lines = [
    `Заходи ко мне в Поигрулькины${room ? `, комната «${room}»` : ""}.`,
    `Код приглашения: ${$("invite-code").textContent}`,
  ];
  const password = call.hostRoom?.password;
  if (password) lines.push(`Пароль: ${password}`);
  return lines.join("\n");
}

$("invite-share").addEventListener("click", () => androidBridge?.share(inviteMessage()));

// Приглашение пришло извне (Android: ссылка или «Поделиться» из
// мессенджера): находим в тексте код и пароль и входим.
function joinFromInvite(text) {
  const code = text?.match(/poi-[A-Za-z0-9_-]+/)?.[0];
  if (!code) return;
  if (call.me) {
    toast("Чтобы войти по приглашению, сначала выйдите из этой комнаты");
    return;
  }
  const password = text.match(/Пароль:[ \t]*(.+)/)?.[1]?.trim() ?? "";
  openManual();
  $("server").value = code;
  $("manual-pass").value = password;
  // Без имени checkName само подсветит поле, а форма останется заполненной.
  if ($("name").value.trim()) $("manual-form").requestSubmit();
}

window.onAndroidInvite = () => joinFromInvite(androidBridge?.takeInvite());

// ---------------------------------------------------------------------------
// Версия программы
// ---------------------------------------------------------------------------

// «android 0.1.42» → { platform: "android", version: [0, 1, 42], text: "0.1.42" }.
function parseApp(app) {
  const [platform, text] = (app ?? "").split(" ");
  if (!platform || !/^\d+(\.\d+)*$/.test(text ?? "")) return null;
  return { platform, version: text.split(".").map(Number), text };
}

function newerVersion(a, b) {
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const diff = (a[i] ?? 0) - (b[i] ?? 0);
    if (diff) return diff > 0;
  }
  return false;
}

// О каких версиях уже сказали: подсказка — один раз за запуск программы.
const announcedVersions = new Set();

// Если у кого-то в комнате та же программа (Android или Windows), но новее,
// подсказываем попросить её у него.
function checkNewerApp() {
  const mine = parseApp(call.me?.app);
  if (!mine) return;
  for (const peer of call.peers.values()) {
    const theirs = parseApp(peer.app);
    if (!theirs || theirs.platform !== mine.platform) continue;
    if (!newerVersion(theirs.version, mine.version) || announcedVersions.has(theirs.text)) continue;
    announcedVersions.add(theirs.text);
    toast(
      `У ${peer.name} программа новее (${theirs.text}). Обновиться: Настройки → «Проверить обновления».`,
    );
    return;
  }
}

// ---------------------------------------------------------------------------
// Обновление программы (app/src/update.rs)
// ---------------------------------------------------------------------------

// Версия новее установленной: { version, notes, apk } или null.
let availableUpdate = null;
// Идёт проверка или скачивание: кнопка не нажимается.
let updateBusy = false;

function setUpdateButton(text, busy = false) {
  updateBusy = busy;
  $("update-btn").textContent = text;
  $("update-btn").disabled = busy;
}

function resetUpdateButton() {
  setUpdateButton(availableUpdate ? `Обновить до ${availableUpdate.version}` : "Проверить обновления");
}

const capitalized = (text) => text.charAt(0).toUpperCase() + text.slice(1);

// quiet — проверка при запуске: говорим только о найденной версии.
async function checkUpdate({ quiet = false } = {}) {
  if (updateBusy) return;
  setUpdateButton("Проверяю…", true);
  let failed = false;
  try {
    availableUpdate = await invoke("check_update");
  } catch (e) {
    failed = true;
    if (!quiet) toast(capitalized(errorText(e)));
  }
  resetUpdateButton();
  if (availableUpdate) {
    toast(
      quiet
        ? `Вышла версия ${availableUpdate.version}. Обновиться: Настройки → «Обновить до ${availableUpdate.version}».`
        : `Есть версия ${availableUpdate.version}. Нажмите «Обновить до ${availableUpdate.version}».`,
    );
  } else if (!quiet && !failed) {
    toast("У вас последняя версия");
  }
}

async function installUpdate() {
  // Установщик закрывает программу, а с ней и комнату — у всех.
  if (call.hosting) {
    toast("Вы хост: сначала закройте комнату, потом обновляйтесь");
    return;
  }
  setUpdateButton("Скачиваю…", true);
  if (isAndroid) {
    androidBridge?.installUpdate(availableUpdate.apk);
    return;
  }
  try {
    // Скачивает, кладёт трубку, ставит и перезапускает программу.
    await invoke("install_update");
  } catch (e) {
    toast(capitalized(errorText(e)));
    resetUpdateButton();
  }
}

listen("update-progress", ({ payload }) => {
  const { downloaded, total } = payload;
  setUpdateButton(total ? `Скачиваю… ${Math.floor((downloaded * 100) / total)} %` : "Скачиваю…", true);
});

// Ход скачивания APK (MainActivity.installUpdate).
window.onAndroidUpdate = (percent) => {
  if (percent === -1) {
    toast("Не удалось скачать обновление. Проверьте интернет и попробуйте ещё раз");
    resetUpdateButton();
  } else if (percent === -2) {
    toast("Разрешите «Поигрулькиным» устанавливать приложения и нажмите «Обновить» ещё раз");
    resetUpdateButton();
  } else if (percent >= 100) {
    // Открылся системный установщик. Он закроет программу — выходим из
    // комнаты сами, чтобы остальные сразу это увидели.
    if (call.me) leave();
    resetUpdateButton();
  } else {
    setUpdateButton(`Скачиваю… ${percent} %`, true);
  }
};

$("update-btn").addEventListener("click", () => {
  if (availableUpdate) installUpdate();
  else checkUpdate();
});

// Проверка при запуске — чуть позже, чтобы не мешать поиску комнат.
setTimeout(() => checkUpdate({ quiet: true }), 5000);

window.__TAURI__?.app
  ?.getVersion()
  .then((version) => ($("app-version").textContent = version))
  .catch(() => {});

$("send-app").addEventListener("click", () => androidBridge?.shareApp());

// Не запоминается: забытый включённым, свой голос в ушах только путает.
$("monitor").addEventListener("change", () => {
  invoke("set_monitor", { enabled: $("monitor").checked });
});

// Переключатель из нескольких кнопок (режим передачи, тема).
function segmented(el, value, onChange) {
  const select = (v) => {
    for (const button of el.querySelectorAll("button")) {
      const active = button.dataset.value === v;
      button.classList.toggle("active", active);
      button.setAttribute("aria-checked", active);
    }
  };
  for (const button of el.querySelectorAll("button")) {
    button.setAttribute("role", "radio");
    button.addEventListener("click", () => {
      select(button.dataset.value);
      onChange(button.dataset.value);
    });
  }
  select(value);
  return select;
}

let currentMode = "voice";

async function setMode(mode) {
  currentMode = mode;
  store.set("mode", mode);
  updateModeRows();
  try {
    await invoke("set_mode", { mode });
  } catch (e) {
    toast(String(e));
  }
}

function updateModeRows() {
  const mode = currentMode;
  $("vad-row").hidden = mode !== "voice";
  $("ptt-row").hidden = mode !== "ptt";
  $("ptt-hint").hidden = mode !== "ptt";
  $("threshold-mark").hidden = mode !== "voice";
  renderDock();
}

$("cues").addEventListener("change", () => {
  const enabled = $("cues").checked;
  store.set("cues", enabled);
  invoke("set_cues", { enabled });
});

$("agc").addEventListener("change", () => {
  const enabled = $("agc").checked;
  store.set("agc", enabled);
  invoke("set_agc", { enabled });
});

$("aec").addEventListener("change", () => {
  const enabled = $("aec").checked;
  store.set("aec", enabled);
  invoke("set_echo_cancellation", { enabled });
});

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
function keyLabel(key) {
  return key.replace(/Key([A-Z])/, "$1").replace(/Digit(\d)/, "$1");
}

function showPttKey(key) {
  $("ptt-key").textContent = key ? keyLabel(key) : "не назначена";
  renderDock();
}

// ---------------------------------------------------------------------------
// Настройки: тема, цвет, устройства
// ---------------------------------------------------------------------------

// Страница настроек: главная или «Оформление».
function showSettingsPage(appearance) {
  $("settings-main").hidden = appearance;
  $("appearance-page").hidden = !appearance;
  $("settings-back").hidden = !appearance;
  $("settings-title").textContent = appearance ? "Оформление" : "Настройки";
  // «Оформление» — небольшая панель без затемнения, чтобы видеть изменения.
  $("settings").classList.toggle("see-through", appearance);
  $("settings").querySelector(".sheet").scrollTop = 0;
}

$("open-appearance").addEventListener("click", () => showSettingsPage(true));
$("settings-back").addEventListener("click", () => showSettingsPage(false));

function openSettings() {
  closeMenus();
  renderProfile();
  showSettingsPage(false);
  $("settings").hidden = false;
  loadDevices();
  refreshDiagnostics();
  clearInterval(diagTimer);
  diagTimer = setInterval(refreshDiagnostics, 1000);
}

function closeSettings() {
  $("settings").hidden = true;
  clearInterval(diagTimer);
  diagTimer = null;
}

// Диагностика звука (только в комнате).
let diagTimer = null;

async function refreshDiagnostics() {
  let stats = null;
  try {
    stats = call.me ? await invoke("audio_stats") : null;
  } catch {
    stats = null;
  }
  $("diag").hidden = !stats;
  if (!stats) return;
  const list = $("diag-list");
  list.innerHTML = "";
  const row = (who, nums, bad, stacked = false) => {
    const li = document.createElement("li");
    li.classList.toggle("stacked", stacked);
    const name = document.createElement("span");
    name.className = "who";
    name.textContent = who;
    name.title = who;
    const value = document.createElement("span");
    value.className = "nums";
    value.classList.toggle("bad", bad);
    value.textContent = nums;
    li.append(name, value);
    list.append(li);
  };
  const khz = (rate) => `${(rate / 1000).toFixed(rate % 1000 ? 1 : 0)} кГц`;
  row(`Микрофон: ${stats.captureDevice}`, `${khz(stats.captureRate)} · сбоев ${stats.captureGlitches}`, stats.captureGlitches > 0);
  row(`Вывод: ${stats.playbackDevice}`, `${khz(stats.playbackRate)} · сбоев ${stats.playbackGlitches}`, stats.playbackGlitches > 0);
  for (const peer of stats.peers) {
    const name = call.peers.get(peer.id)?.name ?? (peer.id === call.me?.id ? "вы (эхо)" : `#${peer.id}`);
    const lostPct = peer.received ? (100 * peer.lost) / peer.received : 0;
    row(
      name,
      `задержка ${Math.round(peer.delayMs)} мс · потери ${lostPct.toFixed(1)}% · опоздали ${peer.late} · лишние ${peer.trimmed}`,
      lostPct > 2 || peer.late > 20,
      true,
    );
  }
  if (stats.peers.length === 0) {
    const li = document.createElement("li");
    li.className = "hint";
    li.textContent = "Собеседники появятся здесь, когда заговорят.";
    list.append(li);
  }
}

for (const button of document.querySelectorAll(".open-settings")) {
  button.addEventListener("click", openSettings);
}
$("settings-close").addEventListener("click", closeSettings);
$("settings").addEventListener("click", (event) => {
  if (event.target === $("settings")) closeSettings();
});
window.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !capturingKey) {
    closeSettings();
    closeShareSheet();
    if ($("address-menu").hidden) closeInvite();
    closeRoomSheet();
    closeMenus();
    if (sv.kind === "camera") closeScreenView();
  }
});

// ---------------------------------------------------------------------------
// Справка «Как это работает»
// ---------------------------------------------------------------------------

function closeHelp() {
  $("help").hidden = true;
}

$("help-btn").addEventListener("click", () => {
  closeMenus();
  $("help").hidden = false;
});
$("help-close").addEventListener("click", closeHelp);
$("help").addEventListener("click", (event) => {
  if (event.target === $("help")) closeHelp();
});
window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeHelp();
});

// ---------------------------------------------------------------------------
// Аватарка
// ---------------------------------------------------------------------------

// Аватарки других участников приходят от сервера (и после переподключения).
listen("peer-avatar", ({ payload }) => {
  if (payload.url) avatars.set(payload.id, payload.url);
  else avatars.delete(payload.id);
  if (!call.me) return;
  renderPeers();
  if (currentTab === "chat") renderChat(false);
});

function renderProfile() {
  paintAvatar($("profile-avatar"), $("name").value.trim() || "?", myAvatar);
  $("avatar-remove").hidden = !myAvatar;
  renderWho();
}

const AVATAR_SIZE = 160;

// Картинку обрезаем до квадрата по центру и уменьшаем до 160×160 JPEG
// (5–20 КБ): так она быстро доходит до всех и не раздувает трафик.
async function makeAvatar(file) {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = AVATAR_SIZE;
  const ctx = canvas.getContext("2d");
  ctx.fillStyle = "#262a33"; // под прозрачные места PNG: в JPEG прозрачности нет
  ctx.fillRect(0, 0, AVATAR_SIZE, AVATAR_SIZE);
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(
    bitmap,
    (bitmap.width - side) / 2,
    (bitmap.height - side) / 2,
    side,
    side,
    0,
    0,
    AVATAR_SIZE,
    AVATAR_SIZE,
  );
  bitmap.close();
  const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/jpeg", 0.85));
  if (!blob) throw new Error("не получилось сжать картинку");
  return Array.from(new Uint8Array(await blob.arrayBuffer()));
}

async function setAvatar(data) {
  try {
    myAvatar = await invoke("set_avatar", { data });
  } catch (e) {
    toast(errorText(e));
    return;
  }
  renderProfile();
  if (call.me) {
    renderPeers();
    if (currentTab === "chat") renderChat(false);
  }
}

$("avatar-file").addEventListener("change", async () => {
  const file = $("avatar-file").files[0];
  $("avatar-file").value = ""; // чтобы та же картинка выбиралась снова
  if (!file) return;
  try {
    await setAvatar(await makeAvatar(file));
  } catch (e) {
    toast(`Не удалось открыть картинку: ${errorText(e)}`);
  }
});

$("avatar-pick").addEventListener("click", () => $("avatar-file").click());
$("me-avatar").addEventListener("click", () => $("avatar-file").click());
$("profile-avatar").addEventListener("click", () => $("avatar-file").click());
$("avatar-remove").addEventListener("click", () => setAvatar([]));

invoke("get_avatar")
  .then((url) => {
    myAvatar = url;
    renderProfile();
    if (call.me) renderPeers();
  })
  .catch(() => {});

// Тема: system — светлая или «Ночь», как в системе; остальные — всегда такие
// (цвета — в style.css). Значение — тёмная ли тема.
const THEMES = {
  light: false,
  dark: true,
  black: true,
  midnight: true,
  forest: true,
  paper: false,
  sakura: false,
  ocean: true,
  sunset: true,
  neon: true,
  terminal: true,
  synthwave: true,
  cyber: true,
  berry: true,
  lemonade: false,
  tangerine: false,
  aqua: false,
};

function applyTheme(theme) {
  if (theme !== "system" && !(theme in THEMES)) theme = "dark";
  store.set("theme", theme);
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  // На Android подложка под системными панелями — в цвет фона темы.
  const dark =
    theme === "system" ? matchMedia("(prefers-color-scheme: dark)").matches : THEMES[theme];
  if (androidBridge?.setThemeColor) {
    const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
    androidBridge.setThemeColor(bg, dark);
  } else {
    androidBridge?.setTheme(dark);
  }
  renderAppearanceSummary();
  return theme;
}

function applyAccent(accent) {
  store.set("accent", accent);
  document.documentElement.dataset.accent = accent;
  for (const button of $("accent").querySelectorAll("button")) {
    button.classList.toggle("active", button.dataset.value === accent);
  }
  renderAppearanceSummary();
}

// Что выбрано в «Оформлении» — подписью под строкой в настройках.
function renderAppearanceSummary() {
  const label = (group, value) =>
    group.querySelector(`[data-value="${value}"]`)?.textContent.trim() ||
    group.querySelector(`[data-value="${value}"]`)?.title || "";
  const parts = [
    label($("theme"), store.get("theme", "dark")),
    label($("accent"), store.get("accent", "indigo")),
  ];
  const bg = store.get("roomBg", "none");
  if (bg !== "none") parts.push(`фон: ${label($("room-bg"), bg).toLowerCase()}`);
  $("appearance-summary").textContent = parts.filter(Boolean).join(" · ");
}

// Фон комнаты — только у себя. Своя картинка уменьшается и хранится в
// localStorage как JPEG (data:-адрес).
const ROOM_BGS = ["none", "aurora", "dusk", "lagoon", "stars", "grid", "image"];
const ROOM_BG_SIZE = 1600;

function applyRoomBg(bg) {
  const image = store.get("roomBgImage", null);
  if (!ROOM_BGS.includes(bg) || (bg === "image" && !image)) bg = "none";
  store.set("roomBg", bg);
  const root = document.documentElement;
  if (bg === "none") delete root.dataset.roomBg;
  else root.dataset.roomBg = bg;
  if (image) root.style.setProperty("--room-image", `url("${image}")`);
  else root.style.removeProperty("--room-image");
  $("room-bg").querySelector('[data-value="image"]').classList.toggle("has-image", Boolean(image));
  selectRoomBg(bg);
  renderAppearanceSummary();
}

async function makeRoomBg(file) {
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, ROOM_BG_SIZE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.round(bitmap.width * scale);
  canvas.height = Math.round(bitmap.height * scale);
  const ctx = canvas.getContext("2d");
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  return canvas.toDataURL("image/jpeg", 0.82);
}

const selectRoomBg = segmented($("room-bg"), store.get("roomBg", "none"), (bg) => {
  // «Своя» без картинки или повторное нажатие на неё — выбрать картинку.
  if (bg === "image") $("room-bg-file").click();
  else applyRoomBg(bg);
});
// Пока картинка не выбрана, «Своя» не должна выглядеть выбранной.
$("room-bg").querySelector('[data-value="image"]').addEventListener("click", () => {
  selectRoomBg(store.get("roomBg", "none"));
});

$("room-bg-file").addEventListener("change", async () => {
  const file = $("room-bg-file").files[0];
  $("room-bg-file").value = "";
  if (!file) return;
  let url;
  try {
    url = await makeRoomBg(file);
  } catch (e) {
    toast(`Не удалось открыть картинку: ${errorText(e)}`);
    return;
  }
  store.set("roomBgImage", url);
  if (store.get("roomBgImage", null) !== url) {
    toast("Картинка не поместилась в настройки, выберите поменьше");
    return;
  }
  applyRoomBg("image");
});

// Размер интерфейса (zoom всей страницы).
const UI_SCALES = ["0.9", "1", "1.1", "1.25"];

function applyScale(scale) {
  if (!UI_SCALES.includes(scale)) scale = "1";
  store.set("uiScale", scale);
  document.documentElement.style.setProperty("--zoom", scale);
}

segmented($("ui-scale"), String(store.get("uiScale", "1")), applyScale);
applyScale(String(store.get("uiScale", "1")));

segmented($("theme"), applyTheme(store.get("theme", "dark")), applyTheme);
for (const button of $("accent").querySelectorAll("button")) {
  button.setAttribute("aria-label", button.title);
  button.addEventListener("click", () => applyAccent(button.dataset.value));
}
applyAccent(store.get("accent", "indigo"));
applyRoomBg(store.get("roomBg", "none"));

// Устройства. kind — "input" (микрофон) или "output" (динамики/наушники);
// value — имя устройства, "" — системное по умолчанию.
const DEVICE_COMMAND = { input: "set_input_device", output: "set_output_device" };

async function chooseDevice(kind, value) {
  const previous = store.get(kind, "");
  store.set(kind, value);
  $(kind).value = value;
  if (!call.me) return; // не в комнате: устройство откроется при подключении
  const pill = $(`${kind}-pill`);
  pill.classList.add("busy");
  try {
    const opened = await invoke(DEVICE_COMMAND[kind], { device: value || null });
    if (opened) showDevice(kind, opened);
  } catch (e) {
    // Новое устройство не открылось — остаётся прежнее.
    store.set(kind, previous);
    $(kind).value = previous;
    toast(`Не удалось переключить устройство: ${errorText(e)}`);
  } finally {
    pill.classList.remove("busy");
  }
}

function showDevice(kind, name) {
  $(`${kind}-pill`).dataset.tip = `${kind === "input" ? "Микрофон" : "Вывод"}: ${name}`;
}

$("input").addEventListener("change", () => chooseDevice("input", $("input").value));
$("output").addEventListener("change", () => chooseDevice("output", $("output").value));

// Android: устройства выбирает система, а человек — только куда идёт звук
// (см. CallAudio.kt). Режим «разговор» включается до подключения, чтобы
// звук сразу открылся с эхоподавлением.
const AUDIO_ROUTES = { auto: "Авто", speaker: "Громкий динамик", earpiece: "Динамик у уха" };

function audioRoute() {
  const route = store.get("audioRoute", "auto");
  return route in AUDIO_ROUTES ? route : "auto";
}

function prepareAudio() {
  stopMicTest(false);
  androidBridge?.prepareCall(audioRoute());
}

// ---------------------------------------------------------------------------
// Проверка микрофона до входа: запись, затем она проигрывается
// ---------------------------------------------------------------------------

const micTest = { timer: null, loudest: -120 };

async function startMicTest() {
  const { input, output } = formValues();
  // Телефон: режим «разговор» и выбранный динамик — как в звонке.
  androidBridge?.prepareCall(audioRoute());
  micTest.loudest = -120;
  $("mic-test-hint").hidden = true;
  try {
    await invoke("mic_test_start", { input, output });
  } catch (e) {
    androidBridge?.callEnded();
    finishMicTest(errorText(e));
    return;
  }
  document.querySelector(".mic-test").classList.add("running");
  clearInterval(micTest.timer);
  micTest.timer = setInterval(refreshMicTest, 80);
}

async function refreshMicTest() {
  let state;
  try {
    state = await invoke("mic_test_status");
  } catch {
    return;
  }
  if (!micTest.timer) return;
  const bar = $("mic-test-bar");
  const label = $("mic-test-label");
  if (state.phase === "recording") {
    micTest.loudest = Math.max(micTest.loudest, state.levelDb);
    bar.style.width = `${dbToPercent(state.levelDb)}%`;
    bar.classList.toggle("active", state.levelDb > -50);
    const left = Math.max(1, Math.ceil(4 * (1 - state.progress)));
    label.textContent = `Скажите что-нибудь… ${left}`;
  } else if (state.phase === "playing") {
    bar.style.width = `${Math.round(state.progress * 100)}%`;
    bar.classList.add("active");
    label.textContent = "Слушайте, как вас слышно";
  } else if (state.phase === "failed") {
    finishMicTest(state.error || "Не удалось открыть микрофон");
  } else {
    finishMicTest(
      micTest.loudest < -55
        ? "Вас почти не слышно. Проверьте, тот ли микрофон выбран в настройках и не выключен ли он."
        : null,
    );
  }
}

// Проверка закончилась: `problem` — что сказать, если что-то не так.
function finishMicTest(problem) {
  stopMicTest(!call.me);
  if (problem) {
    $("mic-test-hint").textContent = problem;
    $("mic-test-hint").classList.add("error");
    $("mic-test-hint").hidden = false;
  }
}

// Останавливает проверку. `endAudio` — на телефоне выйти из режима «разговор»
// (не нужно, если сразу начинается звонок).
function stopMicTest(endAudio) {
  if (!micTest.timer) return;
  clearInterval(micTest.timer);
  micTest.timer = null;
  invoke("mic_test_stop").catch(() => {});
  if (endAudio) androidBridge?.callEnded();
  document.querySelector(".mic-test").classList.remove("running");
  $("mic-test-label").textContent = "Проверить микрофон";
  const bar = $("mic-test-bar");
  bar.style.width = "0";
  bar.classList.remove("active");
  $("mic-test-hint").classList.remove("error");
  $("mic-test-hint").hidden = true;
}

$("mic-test-btn").addEventListener("click", () => {
  if (micTest.timer) stopMicTest(true);
  else startMicTest();
});

const selectAudioRoute = segmented($("audio-route"), audioRoute(), setAudioRoute);

function setAudioRoute(route) {
  store.set("audioRoute", route);
  selectAudioRoute(route);
  androidBridge?.setAudioRoute(route);
  if (isAndroid) $("output-pill").dataset.tip = `Звук: ${AUDIO_ROUTES[route]}`;
}

function openRouteMenu() {
  const menu = $("output-menu");
  menu.innerHTML = "";
  const title = document.createElement("div");
  title.className = "menu-title";
  title.textContent = "Куда идёт звук";
  menu.append(title);
  for (const [value, label] of Object.entries(AUDIO_ROUTES)) {
    const item = document.createElement("button");
    item.type = "button";
    item.classList.toggle("current", value === audioRoute());
    const check = icon("check");
    check.classList.add("check");
    const text = document.createElement("span");
    text.className = "label";
    text.textContent = label;
    item.append(check, text);
    item.addEventListener("click", () => {
      closeMenus();
      setAudioRoute(value);
    });
    menu.append(item);
  }
  menu.hidden = false;
}

// Быстрый выбор устройства прямо в комнате.
async function openDeviceMenu(kind) {
  const menu = $(`${kind}-menu`);
  const wasOpen = !menu.hidden;
  closeMenus();
  if (wasOpen) return;
  if (isAndroid) return openRouteMenu();
  await loadDevices(); // список мог измениться: подключили наушники
  if (!devices) return;
  const names = kind === "input" ? devices.inputs : devices.outputs;
  const systemDefault = kind === "input" ? devices.defaultInput : devices.defaultOutput;
  const current = savedDevice(names, store.get(kind, ""));

  menu.innerHTML = "";
  const title = document.createElement("div");
  title.className = "menu-title";
  title.textContent = kind === "input" ? "Микрофон" : "Динамики / наушники";
  menu.append(title);
  const options = [["", systemDefault ? `По умолчанию (${systemDefault})` : "По умолчанию"]];
  for (const name of names) options.push([name, name]);
  for (const [value, label] of options) {
    const item = document.createElement("button");
    item.type = "button";
    item.classList.toggle("current", value === current || (value === "" && !names.includes(current)));
    const check = icon("check");
    check.classList.add("check");
    const text = document.createElement("span");
    text.className = "label";
    text.textContent = label;
    item.title = label;
    item.append(check, text);
    item.addEventListener("click", () => {
      closeMenus();
      chooseDevice(kind, value);
    });
    menu.append(item);
  }
  menu.hidden = false;
}

for (const kind of ["input", "output"]) {
  $(`${kind}-pill`).addEventListener("click", (event) => {
    event.stopPropagation();
    openDeviceMenu(kind);
  });
  $(`${kind}-menu`).addEventListener("click", (event) => event.stopPropagation());
}

// ---------------------------------------------------------------------------
// Трей
// ---------------------------------------------------------------------------

$("close-to-tray").addEventListener("change", () => {
  const enabled = $("close-to-tray").checked;
  store.set("closeToTray", enabled);
  invoke("set_close_to_tray", { enabled });
});

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

async function init() {
  $("name").value = store.get("name", "");
  renderWho();

  let mode = store.get("mode", "voice");
  if (isAndroid && mode === "ptt") mode = "voice"; // клавиши рации на телефоне нет
  const vad = store.get("vad", -45);
  const pttKey = store.get("pttKey", null);
  const denoise = store.get("denoise", true);
  const agc = store.get("agc", true);
  $("agc").checked = agc;
  const aec = store.get("aec", true);
  $("aec").checked = aec;
  const cues = store.get("cues", true);
  $("cues").checked = cues;
  const closeToTray = store.get("closeToTray", true);
  $("close-to-tray").checked = closeToTray;
  currentMode = mode;
  segmented($("mode"), mode, setMode);
  $("denoise").checked = denoise;
  $("vad").value = vad;
  showVad(vad);
  showPttKey(pttKey);
  updateModeRows();

  startLanSearch();
  loadRooms();
  await loadDevices();
  try {
    await invoke("set_noise_suppression", { enabled: denoise });
    await invoke("set_cues", { enabled: cues });
    await invoke("set_agc", { enabled: agc });
    await invoke("set_echo_cancellation", { enabled: aec });
    await invoke("set_close_to_tray", { enabled: closeToTray });
    await invoke("set_vad_threshold", { db: vad });
    await invoke("set_ptt_key", { key: pttKey });
    await invoke("set_mode", { mode });
  } catch (e) {
    toast(String(e));
  }
  // Программу открыли ссылкой-приглашением или «Поделиться».
  if (androidBridge) window.onAndroidInvite();
}

init();
