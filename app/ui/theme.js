// Тема и цвет оформления. Подключается в <head>, до отрисовки страницы,
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
  if (theme === "light" || theme === "dark") root.dataset.theme = theme;
  root.dataset.accent = read("accent", "indigo");
  // Телефон: без трея, рации по клавише, показа экрана и своих комнат.
  if (/Android/i.test(navigator.userAgent)) root.classList.add("android");
})();
