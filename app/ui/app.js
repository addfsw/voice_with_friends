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
  // Сведения о своём сервере, если комнату открыли мы.
  hosting: null,
  hostRoom: null, // настройки открытой своей комнаты: { id, name, lan, internet }
  me: null, // мы сами: { id, name, admin, muted, deafened, serverMuted }
  peers: new Map(), // id -> такие же сведения о собеседнике
  statusTimer: null,
  chat: [], // сообщения чата: { id, from, name, text, timeMs }, старые — первыми
  chatPending: [], // пришли, пока интерфейс ещё не вошёл в комнату
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
  "view-list": "M3 13h2v-2H3v2Zm0 4h2v-2H3v2Zm0-8h2V7H3v2Zm4 4h14v-2H7v2Zm0 4h14v-2H7v2ZM7 7v2h14V7H7Z",
  "view-grid":
    "M3 3v8h8V3H3Zm6 6H5V5h4v4Zm-6 4v8h8v-8H3Zm6 6H5v-4h4v4Zm4-16v8h8V3h-8Zm6 6h-4V5h4v4Zm-6 4v8h8v-8h-8Zm6 6h-4v-4h4v4Z",
  "person-add":
    "M15 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8Zm-9-2V7H4v3H1v2h3v3h2v-3h3v-2H6Zm9 4c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4Z",
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

    const dot = document.createElement("span");
    dot.className = "lan-dot";
    const name = document.createElement("span");
    name.className = "lan-name";
    name.textContent = server.name;
    const meta = document.createElement("span");
    meta.className = "lan-meta";
    meta.textContent = server.compatible
      ? `${server.host} · ${server.users} чел.`
      : `${server.host} · другая версия программы`;
    button.append(dot, name, meta);

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
    : "Не найдено. Друзья через интернет вставляют сюда код приглашения от хоста.";
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
  // По коду приглашения подключение дольше: встреча через ящик и пробивка NAT.
  button.textContent = isInvite(server) ? "Связываюсь с хостом через интернет…" : "Подключение…";
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
    $("connect-form").requestSubmit();
  } catch (e) {
    toast(`Не удалось запомнить сертификат: ${errorText(e)}`);
  }
});

// ---------------------------------------------------------------------------
// Свои комнаты: сервер запускается прямо в программе
// ---------------------------------------------------------------------------

let myRooms = [];
let roomsBusy = false;

async function loadRooms() {
  try {
    myRooms = await invoke("list_rooms");
  } catch (e) {
    toast(`Не удалось загрузить список комнат: ${errorText(e)}`);
  }
  renderRooms();
}

function roomMeta(room) {
  const parts = [room.lan ? "видна в сети" : "скрыта из сети"];
  if (room.internet) parts[0] = room.lan ? "сеть + интернет" : "только интернет";
  return parts.join(" · ");
}

function renderRooms(busyId = null) {
  const list = $("my-rooms");
  list.innerHTML = "";
  for (const room of myRooms) {
    const li = document.createElement("li");
    li.classList.toggle("busy", room.id === busyId);
    const open = document.createElement("button");
    open.type = "button";
    open.className = "lan-item";
    open.disabled = roomsBusy;
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
    meta.textContent = room.id === busyId ? "Запуск сервера…" : roomMeta(room);
    text.append(name, meta);
    open.append(badge, text);
    open.addEventListener("click", () => openRoom(room));
    li.append(open, roomMenu(room, li));
    list.append(li);
  }
  $("new-room-btn").disabled = roomsBusy;
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
  button.disabled = roomsBusy;
  const menu = document.createElement("div");
  menu.className = "menu";
  menu.hidden = true;

  const rename = document.createElement("button");
  rename.type = "button";
  rename.textContent = "Переименовать";
  rename.addEventListener("click", () => {
    closeMenus();
    editRoomName(li, room.name, async (name) => {
      const updated = await invoke("rename_room", { id: room.id, name });
      myRooms = myRooms.map((r) => (r.id === room.id ? updated : r));
    });
  });
  menu.append(rename);

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

// Поле для названия на месте строки комнаты. Enter — сохранить, Esc — отмена.
function editRoomName(li, value, save) {
  const row = document.createElement("div");
  row.className = "room-edit";
  const input = document.createElement("input");
  input.maxLength = 40;
  input.value = value;
  const ok = document.createElement("button");
  ok.type = "button";
  ok.className = "primary";
  ok.textContent = "Сохранить";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "icon-btn ghost";
  cancel.title = "Отмена";
  cancel.setAttribute("aria-label", "Отмена");
  cancel.append(icon("close"));
  row.append(input, ok, cancel);

  const submit = async () => {
    if (!input.value.trim()) return input.focus();
    try {
      await save(input.value);
      renderRooms();
    } catch (e) {
      toast(errorText(e));
    }
  };
  ok.addEventListener("click", submit);
  cancel.addEventListener("click", () => renderRooms());
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      event.preventDefault(); // иначе отправится форма подключения
      submit();
    } else if (event.key === "Escape") {
      renderRooms();
    }
  });
  li.replaceChildren(row);
  input.focus();
  input.select();
}

function showNewRoom(show) {
  $("new-room").hidden = !show;
  $("new-room-btn").hidden = show;
  $("new-room-name").value = "";
  if (show) $("new-room-name").focus();
}

async function createRoom() {
  const name = $("new-room-name").value;
  if (!name.trim()) return $("new-room-name").focus();
  try {
    myRooms.push(await invoke("create_room", { name }));
    showNewRoom(false);
    renderRooms();
  } catch (e) {
    toast(errorText(e));
  }
}

$("new-room-btn").addEventListener("click", () => showNewRoom(true));
$("new-room-cancel").addEventListener("click", () => showNewRoom(false));
$("new-room-create").addEventListener("click", createRoom);
$("new-room-name").addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    createRoom();
  } else if (event.key === "Escape") {
    showNewRoom(false);
  }
});

// Открываем свою комнату: запускаем сервер и подключаемся к нему сами.
async function openRoom(room) {
  if (roomsBusy) return;
  if (!$("name").value.trim()) {
    $("name").reportValidity();
    return;
  }
  const { name, input, output } = formValues();
  const error = $("connect-error");
  roomsBusy = true;
  renderRooms(room.id);
  error.hidden = true;
  let hosted = null;
  try {
    hosted = await invoke("start_server", { roomId: room.id });
    const joined = await invoke("connect", {
      args: { server: hosted.localAddress, name, input, output },
    });
    call.hosting = hosted.info;
    call.hostRoom = hosted.room;
    enterRoom(hosted.localAddress, name, joined);
  } catch (e) {
    if (hosted) await invoke("stop_server").catch(() => {});
    error.textContent = errorText(e);
    error.hidden = false;
  } finally {
    roomsBusy = false;
    renderRooms();
  }
}

// ---------------------------------------------------------------------------
// Комната
// ---------------------------------------------------------------------------

function enterRoom(server, name, joined) {
  call.server = server;
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
}

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

// Нижняя панель: кто вы, состояние и иконки управления.
function renderDock() {
  const me = call.me;
  if (!me) return;
  $("me-avatar").textContent = initial(me.name);
  $("me-avatar").style.background = avatarColor(me.name);
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
  renderChat(mine);
});

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
      avatar.style.background = avatarColor(message.name);
      avatar.textContent = initial(message.name);
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
    invoke("open_viewer", { id: peer.id, name: peer.name }).catch((e) => toast(errorText(e)));
  });
  return button;
}

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
    closeLeaveSheet();
    closeMenus();
  }
});

// Тема: system — как в Windows; light/dark — всегда такая.
function applyTheme(theme) {
  store.set("theme", theme);
  if (theme === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = theme;
}

function applyAccent(accent) {
  store.set("accent", accent);
  document.documentElement.dataset.accent = accent;
  for (const button of $("accent").querySelectorAll("button")) {
    button.classList.toggle("active", button.dataset.value === accent);
  }
}

segmented($("theme"), store.get("theme", "dark"), applyTheme);
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

// Быстрый выбор устройства прямо в комнате.
async function openDeviceMenu(kind) {
  const menu = $(`${kind}-menu`);
  const wasOpen = !menu.hidden;
  closeMenus();
  if (wasOpen) return;
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
  $("server").value = store.get("server", "");
  $("name").value = store.get("name", "");

  const mode = store.get("mode", "voice");
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
}

init();
