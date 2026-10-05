// Окно просмотра чужого экрана (см. src/screen.rs). Кадры показывает
// screen-view.js — тот же, что у просмотра на телефоне.

const { getCurrentWindow } = window.__TAURI__.window;

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
const peer = Number(params.get("id"));
const name = params.get("name") || "участник";

$("title").textContent = `Экран: ${name}`;
$("waiting-text").textContent = `Жду картинку от ${name}…`;

// ---------------------------------------------------------------------------
// Счётчик кадров в секунду
// ---------------------------------------------------------------------------

let shownTimes = [];
let size = "";

function frameShown(width, height) {
  $("waiting").hidden = true;
  const now = performance.now();
  shownTimes.push(now);
  shownTimes = shownTimes.filter((t) => now - t < 1000);
  size = `${width}×${height}`;
}

setInterval(() => {
  const now = performance.now();
  shownTimes = shownTimes.filter((t) => now - t < 1000);
  if (size) $("info").textContent = `${size} · ${shownTimes.length} к/с`;
}, 500);

createScreenView({ peer, canvas: $("canvas"), img: $("screen"), onFrame: frameShown });

// ---------------------------------------------------------------------------
// На весь экран: кнопка, двойной щелчок, F11; выход — Esc.
// ---------------------------------------------------------------------------

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
    if (!$("waiting").hidden) return;
    $("bar").classList.add("hidden");
    document.body.classList.add("idle");
  }, 2500);
}
window.addEventListener("mousemove", wake);
wake();
