// Только в релизной сборке (см. main.rs): окна программы ведут себя как
// приложение, а не как страница браузера. Меню WebView по правому клику
// («Обновить», «Печать»…) и клавиши перезагрузки отключены — перезагрузка
// сбрасывала интерфейс посреди звонка.
(() => {
  const editable = (target) =>
    target instanceof Element && target.closest("input, textarea, [contenteditable]");

  window.addEventListener(
    "contextmenu",
    (event) => {
      // В полях ввода и на выделенном тексте меню нужно: копировать, вставить.
      if (editable(event.target) || String(window.getSelection() ?? "")) return;
      event.preventDefault();
    },
    true,
  );

  window.addEventListener(
    "keydown",
    (event) => {
      const ctrl = event.ctrlKey || event.metaKey;
      const blocked =
        event.code === "F5" || // обновить
        event.code === "F7" || // режим каретки
        event.code === "F12" || // инструменты разработчика
        event.code === "BrowserRefresh" ||
        (ctrl && ["KeyR", "KeyP", "KeyU", "KeyF", "KeyG"].includes(event.code)) ||
        (ctrl && event.shiftKey && ["KeyI", "KeyJ", "KeyC"].includes(event.code)) ||
        (event.altKey && ["ArrowLeft", "ArrowRight"].includes(event.code)); // назад/вперёд
      if (blocked) event.preventDefault();
    },
    true,
  );
})();
