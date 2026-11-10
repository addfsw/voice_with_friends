// Тема, цвет и остальное оформление. Подключается в <head>, до отрисовки страницы,
// чтобы при запуске окно сразу было нужного цвета. Выбор меняет app.js.
(() => {
  const read = (key, fallback) => {
    try {
      const raw = localStorage.getItem(key);
      return raw === null ? fallback : JSON.parse(raw);
    } catch {
      return fallback;
    }
  };
  const root = document.documentElement;
  const theme = read("theme", "dark");
  // Список тем — THEMES в app.js; "system" — без data-theme.
  if (theme !== "system") root.dataset.theme = theme;
  root.dataset.accent = read("accent", "indigo");
  // Фон комнаты и размер — тоже до отрисовки (проверяет app.js).
  const bg = read("roomBg", "none");
  const image = read("roomBgImage", null);
  if (bg !== "none" && (bg !== "image" || image)) root.dataset.roomBg = bg;
  if (image) root.style.setProperty("--room-image", `url("${image}")`);
  root.style.setProperty("--zoom", read("uiScale", "1"));
  // Телефон: без трея, рации по клавише, показа экрана и своих комнат.
  if (/Android/i.test(navigator.userAgent)) root.classList.add("android");
})();
