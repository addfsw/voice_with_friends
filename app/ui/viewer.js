// Окно просмотра чужого экрана (см. src/screen.rs).
//
// Основной путь: кадры H.264 приходят в канал как есть, и окно декодирует их
// видеокартой (WebCodecs) прямо на холст. Если декодера нет или он не
// справился — запасной путь: кадры декодирует Rust в JPEG, о новом кадре
// приходит событие screen-frame, а картинка загружается по адресу screen://.

const { listen } = window.__TAURI__.event;
const { invoke, convertFileSrc, Channel } = window.__TAURI__.core;
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

// ---------------------------------------------------------------------------
// Основной путь: WebCodecs
// ---------------------------------------------------------------------------

const VIDEO_HEADER = 9; // [номер кадра u32][опорный u8][ширина u16][высота u16]

let decoder = null;
let codec = null;
let expected = null; // номер следующего кадра
let waitingKeyframe = true;
let lastAsk = 0;
let lastOutput = 0;
let firstPacket = 0;
let failures = 0;
let direct = false;

async function webCodecsWorks() {
  if (!("VideoDecoder" in window)) return false;
  try {
    const support = await VideoDecoder.isConfigSupported({ codec: "avc1.42E01F" });
    return support.supported === true;
  } catch {
    return false;
  }
}

function toBytes(message) {
  if (message instanceof ArrayBuffer) return new Uint8Array(message);
  if (ArrayBuffer.isView(message)) {
    return new Uint8Array(message.buffer, message.byteOffset, message.byteLength);
  }
  return Uint8Array.from(message);
}

// Кодек вида avc1.PPCCLL — из SPS опорного кадра (профиль, ограничения, уровень).
function codecFromSps(data) {
  for (let i = 0; i + 7 < data.length; i++) {
    const startCode = data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1;
    if (startCode && (data[i + 3] & 0x1f) === 7) {
      const hex = (b) => b.toString(16).padStart(2, "0").toUpperCase();
      return `avc1.${hex(data[i + 4])}${hex(data[i + 5])}${hex(data[i + 6])}`;
    }
  }
  return null;
}

function askKeyframe() {
  waitingKeyframe = true;
  const now = performance.now();
  if (now - lastAsk > 500) {
    lastAsk = now;
    invoke("request_keyframe", { id: peer }).catch(() => {});
  }
}

const canvas = $("canvas");
const ctx = canvas.getContext("2d");

function onDecoded(frame) {
  lastOutput = performance.now();
  failures = 0;
  if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
    canvas.width = frame.displayWidth;
    canvas.height = frame.displayHeight;
  }
  ctx.drawImage(frame, 0, 0);
  canvas.hidden = false;
  frameShown(frame.displayWidth, frame.displayHeight);
  frame.close();
}

function onDecoderError(error) {
  console.warn("декодер:", error);
  decoder = null;
  failures += 1;
  if (failures >= 3) useJpeg("декодер видеокарты выдаёт ошибки");
  else askKeyframe();
}

function configure(newCodec) {
  if (decoder && decoder.state !== "closed") decoder.close();
  codec = newCodec;
  decoder = new VideoDecoder({ output: onDecoded, error: onDecoderError });
  // Без description декодер ждёт поток Annex B — именно такой даёт OpenH264.
  decoder.configure({ codec, optimizeForLatency: true });
}

function onPacket(message) {
  if (!direct) return;
  const packet = toBytes(message);
  if (packet.length <= VIDEO_HEADER) return;
  const view = new DataView(packet.buffer, packet.byteOffset, packet.byteLength);
  const frameNo = view.getUint32(0, true);
  const keyframe = packet[4] !== 0;
  const data = packet.subarray(VIDEO_HEADER);
  if (!firstPacket) firstPacket = performance.now();

  // Пропущенный кадр ломает все следующие до опорного.
  const gap = expected !== null && frameNo !== expected;
  if (!keyframe && (waitingKeyframe || gap || !decoder)) {
    askKeyframe();
    return;
  }
  expected = (frameNo + 1) >>> 0;
  if (keyframe) {
    const spsCodec = codecFromSps(data) ?? codec ?? "avc1.42E01F";
    if (!decoder || decoder.state === "closed" || spsCodec !== codec) configure(spsCodec);
    waitingKeyframe = false;
  }
  try {
    decoder.decode(
      new EncodedVideoChunk({
        type: keyframe ? "key" : "delta",
        timestamp: Math.round(performance.now() * 1000),
        data,
      }),
    );
  } catch (e) {
    onDecoderError(e);
  }
}

// Страховка: кадры идут, а на экране ничего — переходим на JPEG.
setInterval(() => {
  if (!direct || !firstPacket) return;
  const now = performance.now();
  const since = Math.max(firstPacket, lastOutput);
  if (now - since > 4000) useJpeg("видеокарта не показала ни кадра");
}, 1000);

// ---------------------------------------------------------------------------
// Запасной путь: JPEG
// ---------------------------------------------------------------------------

let jpeg = false;
let loading = false;
let pending = null;

function load(frame) {
  loading = true;
  $("screen").src = convertFileSrc(`${peer}/${frame.n}`, "screen");
  $("screen").dataset.width = frame.width;
  $("screen").dataset.height = frame.height;
}

$("screen").addEventListener("load", () => {
  loading = false;
  $("screen").hidden = false;
  frameShown($("screen").dataset.width, $("screen").dataset.height);
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
  if (!jpeg || payload.id !== peer) return;
  if (loading) pending = payload;
  else load(payload);
});

async function useJpeg(reason) {
  if (jpeg) return;
  if (reason) console.warn(`переключаюсь на JPEG: ${reason}`);
  direct = false;
  jpeg = true;
  if (decoder && decoder.state !== "closed") decoder.close();
  decoder = null;
  canvas.hidden = true;
  await invoke("subscribe_screen", { id: peer, direct: false, channel: new Channel() });
}

async function start() {
  if (await webCodecsWorks()) {
    direct = true;
    const channel = new Channel();
    channel.onmessage = onPacket;
    await invoke("subscribe_screen", { id: peer, direct: true, channel });
  } else {
    await useJpeg();
  }
}

start();

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
