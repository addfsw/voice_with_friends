// Окно просмотра чужого экрана. Кадры готовит Rust (src/screen.rs): о новом
// кадре приходит событие screen-frame, а саму картинку окно загружает по
// адресу своего протокола screen://.

const { listen } = window.__TAURI__.event;
const { convertFileSrc } = window.__TAURI__.core;
const { getCurrentWindow } = window.__TAURI__.window;

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
const peer = Number(params.get("id"));
const name = params.get("name") || "участник";

$("title").textContent = `Экран: ${name}`;
$("waiting-text").textContent = `Жду картинку от ${name}…`;

// Загружаем по одному кадру: пока грузится предыдущий, новые не копим,
// а берём самый свежий.
let loading = false;
let pending = null;
let shown = 0;
let fpsWindow = [];

function load(frame) {
  loading = true;
  $("screen").src = convertFileSrc(`${peer}/${frame.n}`, "screen");
  $("screen").dataset.size = `${frame.width}×${frame.height}`;
}

$("screen").addEventListener("load", () => {
  loading = false;
  $("screen").hidden = false;
  $("waiting").hidden = true;
  const now = performance.now();
  fpsWindow.push(now);
  fpsWindow = fpsWindow.filter((t) => now - t < 1000);
  shown += 1;
  if (shown % 5 === 0) {
    $("info").textContent = `${$("screen").dataset.size} · ${fpsWindow.length} к/с`;
  }
  if (pending) {
    const next = pending;
    pending = null;
    load(next);
  }
});

$("screen").addEventListener("error", () => {
  loading = false;
});

listen("screen-frame", ({ payload }) => {
  if (payload.id !== peer) return;
  if (loading) pending = payload;
  else load(payload);
});

// На весь экран: кнопка, двойной щелчок, F11; выход — Esc.
const appWindow = getCurrentWindow();

async function toggleFullscreen() {
  const full = !(await appWindow.isFullscreen());
  await appWindow.setFullscreen(full);
  $("fullscreen").textContent = full ? "Обычный размер" : "На весь экран";
}

$("fullscreen").addEventListener("click", toggleFullscreen);
$("stage").addEventListener("dblclick", toggleFullscreen);
window.addEventListener("keydown", async (event) => {
  if (event.key === "F11") {
    event.preventDefault();
    toggleFullscreen();
  } else if (event.key === "Escape" && (await appWindow.isFullscreen())) {
    toggleFullscreen();
  }
});

// Панель и курсор прячутся, если мышь не двигается.
let idleTimer = null;
function wake() {
  $("bar").classList.remove("hidden");
  document.body.classList.remove("idle");
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => {
    if ($("screen").hidden) return;
    $("bar").classList.add("hidden");
    document.body.classList.add("idle");
  }, 2500);
}
window.addEventListener("mousemove", wake);
wake();
