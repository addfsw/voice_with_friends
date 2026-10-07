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
  chat: [], // сообщения чата: { id, from, name, text, timeMs }, старые — первыми
  chatPending: [], // пришли, пока интерфейс ещё не вошёл в комнату
  title: "", // название комнаты для уведомлений Android ("" — неизвестно)
  unread: 0,
  sharing: false, // мы показываем экран
  viewers: 0, // сколько человек смотрит наш экран
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
  rotate:
    "M16.48 2.52c3.27 1.55 5.61 4.72 5.97 8.48h1.5C23.44 4.84 18.29 0 12 0l-.66.03 3.81 3.81 1.33-1.32Zm-6.25-.77a1.49 1.49 0 0 0-2.12 0L1.75 8.11a1.49 1.49 0 0 0 0 2.12l12.02 12.02c.59.59 1.54.59 2.12 0l6.36-6.36c.59-.59.59-1.54 0-2.12L10.23 1.75Zm4.6 19.44L2.81 9.17l6.36-6.36 12.02 12.02-6.36 6.36ZM7.52 21.48C4.25 19.94 1.91 16.76 1.55 13H.05C.56 19.16 5.71 24 12 24l.66-.03-3.81-3.81-1.33 1.32Z",
  window: "M19 4H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V6a2 2 0 0 0-2-2Zm0 14H5V8h14v10Z",
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
  // Запущены ли недавние комнаты: server → "online" | "offline" | "unknown"
  // (проверить не удалось) | "checking".
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
    join.lan = await invoke("discover_servers");
    lanSearched = true;
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
  checkRecent();
  lanTimer = setInterval(refreshLan, LAN_REFRESH_MS);
  roomCheckTimer = setInterval(checkRecent, ROOM_CHECK_MS);
}

function stopLanSearch() {
  clearInterval(lanTimer);
  clearInterval(roomCheckTimer);
  lanTimer = null;
  roomCheckTimer = null;
}

// Недавние комнаты проверяются в фоне: запущены ли они сейчас. Проверка по
// коду приглашения идёт через тот же почтовый ящик, что и вход, поэтому
// не чаще раза в минуту.
const ROOM_CHECK_MS = 60000;
let roomCheckTimer = null;
let roomChecking = false;

async function checkRecent(servers = null) {
  if (roomChecking || $("connect-view").hidden) return;
  const found = new Set(join.lan.map((s) => s.address));
  servers ??= recentRooms()
    .map((r) => r.server)
    .filter((server) => !found.has(server));
  if (!servers.length) return;
  roomChecking = true;
  for (const server of servers) {
    if (!join.status.has(server)) join.status.set(server, "checking");
  }
  if (!join.prompt && !join.busy) renderJoin();
  try {
    for (const { server, status } of await invoke("check_rooms", { servers })) {
      join.status.set(server, status);
      // Хост открыл комнату — старое «не запущена» под названием больше не верно.
      if (status !== "offline" && join.errors.get(server) === OFFLINE_TEXT) {
        join.errors.delete(server);
      }
    }
  } catch {
    for (const server of servers) join.status.set(server, "unknown");
  } finally {
    roomChecking = false;
    if (!join.prompt && !join.busy) renderJoin();
  }
}

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
    [{ status }] = await invoke("check_rooms", { servers: [target.server] });
  } catch {}
  join.status.set(target.server, status);
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
  for (const target of targets) list.append(joinCard(target, busy));

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
  const status = target.kind === "recent" ? join.status.get(target.server) : null;
  if (status) {
    ico.classList.add(status);
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
    const state = ROOM_STATE[status];
    if (state) {
      const label = document.createElement("span");
      label.className = `room-state ${status}`;
      label.textContent = state;
      meta.append(label, " · ");
    }
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
    join.status.get(target.server) === "offline" &&
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
    join.status.set(target.server, "online");
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
      if (target.kind === "recent") setTimeout(() => checkRecent([target.server]));
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
}

// Сведения о комнате после (пере)подключения.
function applyJoined(joined) {
  const server = call.server;
  call.me = joined.me;
  call.peers = new Map(joined.peers.map((p) => [p.id, p]));
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
  closeMenus();
  closeInvite();
  $("leave-sheet").hidden = true;
  stopInternetPolling();
  clearInterval(call.statusTimer);
  call.statusTimer = null;
  call.peers.clear();
  showView("connect");
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

// Хосту закрытие комнаты отключает всех, поэтому сначала спрашиваем.
$("leave-btn").addEventListener("click", () => {
  if (!call.hosting) return leave();
  closeInvite();
  $("leave-sheet").hidden = false;
  $("leave-confirm").focus();
});

function closeLeaveSheet() {
  $("leave-sheet").hidden = true;
}

$("leave-confirm").addEventListener("click", () => {
  closeLeaveSheet();
  leave();
});
$("leave-cancel").addEventListener("click", closeLeaveSheet);
$("leave-close").addEventListener("click", closeLeaveSheet);
$("leave-sheet").addEventListener("click", (event) => {
  if (event.target === $("leave-sheet")) closeLeaveSheet();
});

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
  $("echo").checked = false; // и без эха
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
  button.dataset.tip = locked ? "Вход закрыт. Нажмите, чтобы открыть" : "Никого больше не пускать";
  button.replaceChildren(icon(locked ? "lock" : "lock-open"));
  $("room-locked").hidden = !locked;
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
  // Участник закончил показ или ушёл — закрываем его экран.
  if (sv.view && !call.peers.get(sv.peer)?.sharing) closeScreenView();
  renderDock();
  const list = $("peers");
  list.innerHTML = "";
  list.append(peerItem(call.me, true));
  for (const peer of [...call.peers.values()].sort((a, b) => a.id - b.id)) {
    list.append(peerItem(peer, false));
  }
  $("people-count").textContent = String(call.peers.size + 1);
  if (call.peers.size === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = call.hosting
      ? "Пока никого. Позовите друзей кнопкой «Пригласить» вверху."
      : "Пока никого нет. Позовите друзей!";
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
  paintAvatar(avatar, peer.name, isMe ? myAvatar : avatars.get(peer.id));

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
  if (!isMe) main.append(volumeControl(peer.id, peer.name));

  const side = document.createElement("div");
  side.className = "peer-side";
  if (peer.sharing) side.append(watchButton(peer, isMe));
  // Статус — просто значки; свои микрофон и звук переключаются внизу.
  if (peer.muted) side.append(statusIcon(icon("mic", true), isMe ? "Микрофон выключен" : "Выключил микрофон"));
  if (peer.deafened) side.append(statusIcon(icon("headphones", true), isMe ? "Звук выключен" : "Выключил звук"));
  if (!isMe) {
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
  $("address-btn").setAttribute("aria-expanded", "false");
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
  avatars.delete(payload.id);
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
  if (!isMe) button.append(document.createTextNode("Смотреть"));
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
  $("sv-title").textContent = `Экран: ${peer.name}`;
  $("sv-waiting-text").textContent = `Жду картинку от ${peer.name}…`;
  $("sv-waiting").hidden = false;
  resetZoom();
  showBar();
  $("screen-view").hidden = false;
  androidBridge?.setImmersive(true);
  sv.view = createScreenView({
    peer: peer.id,
    canvas: $("sv-canvas"),
    img: $("sv-img"),
    onFrame: () => ($("sv-waiting").hidden = true),
  });
}

function closeScreenView() {
  if (!sv.view) return false;
  sv.view.stop();
  invoke("stop_watching", { id: sv.peer }).catch(() => {});
  sv.view = null;
  sv.peer = null;
  $("screen-view").hidden = true;
  androidBridge?.setImmersive(false);
  if (sv.landscape) setLandscape(false);
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
}


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
  } else if (status.symmetric) {
    state.textContent =
      `Внешний адрес ${status.public}, но ваш роутер выдаёт разным собеседникам разные порты ` +
      "(«симметричный» NAT). Друзья, скорее всего, не подключатся — нужен Tailscale.";
    state.classList.add("warning-text");
  } else {
    const guests = status.guests ? ` · подключений по коду: ${status.guests}` : "";
    state.textContent = `Готово. Внешний адрес ${status.public}${guests}`;
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

$("echo").addEventListener("change", () => {
  invoke("set_echo", { enabled: $("echo").checked });
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

function openSettings() {
  closeMenus();
  renderProfile();
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
  // На телефоне трея нет — без разговора в «Дополнительно» пусто.
  if (isAndroid) $("advanced").hidden = !stats;
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
    closeLeaveSheet();
    closeMenus();
  }
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

// Тема: system — как в Windows; light/dark — всегда такая.
function applyTheme(theme) {
  store.set("theme", theme);
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
  // На Android подложка под системными панелями — в цвет темы.
  const dark =
    theme === "dark" || (theme === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  androidBridge?.setTheme(dark);
}

function applyAccent(accent) {
  store.set("accent", accent);
  document.documentElement.dataset.accent = accent;
  for (const button of $("accent").querySelectorAll("button")) {
    button.classList.toggle("active", button.dataset.value === accent);
  }
}

segmented($("theme"), store.get("theme", "dark"), applyTheme);
if (androidBridge) applyTheme(store.get("theme", "dark"));
for (const button of $("accent").querySelectorAll("button")) {
  button.setAttribute("aria-label", button.title);
  button.addEventListener("click", () => applyAccent(button.dataset.value));
}
applyAccent(store.get("accent", "indigo"));

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
  androidBridge?.prepareCall(audioRoute());
}

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
