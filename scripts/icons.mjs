// Рендерит иконки программы из app/icons/icon.svg (и icon-small.svg для
// 16 и 24 px): PNG для Tauri и icon.ico для Windows. Иконка Android —
// векторная, в app/gen/android/.../res/drawable/ic_launcher_*.xml.
//
// Нужен Playwright с Chromium: npx playwright install chromium
// Запуск из корня репозитория: node scripts/icons.mjs

import { readFileSync, writeFileSync } from "node:fs";
import { chromium } from "playwright";

const dir = new URL("../app/icons/", import.meta.url);
const big = readFileSync(new URL("icon.svg", dir), "utf8");
const small = readFileSync(new URL("icon-small.svg", dir), "utf8");

const browser = await chromium.launch(
  process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {},
);
const page = await browser.newPage();

async function render(svg, size) {
  await page.setViewportSize({ width: size, height: size });
  await page.setContent(
    `<style>html,body{margin:0;background:transparent}svg{display:block;width:${size}px;height:${size}px}</style>${svg}`,
  );
  return page.screenshot({ omitBackground: true, type: "png" });
}

const svgFor = (size) => (size <= 24 ? small : big);

for (const [name, size] of [
  ["32x32.png", 32],
  ["128x128.png", 128],
  ["128x128@2x.png", 256],
  ["icon.png", 1024],
]) {
  writeFileSync(new URL(name, dir), await render(svgFor(size), size));
}

// ICO: заголовок, каталог и PNG-картинки подряд (так умеет Windows Vista+).
const sizes = [16, 24, 32, 48, 64, 128, 256];
const images = [];
for (const size of sizes) images.push(await render(svgFor(size), size));
const header = Buffer.alloc(6 + 16 * sizes.length);
header.writeUInt16LE(0, 0);
header.writeUInt16LE(1, 2); // 1 — иконка
header.writeUInt16LE(sizes.length, 4);
let offset = header.length;
sizes.forEach((size, i) => {
  const entry = 6 + 16 * i;
  header.writeUInt8(size % 256, entry); // 0 означает 256
  header.writeUInt8(size % 256, entry + 1);
  header.writeUInt16LE(1, entry + 4); // плоскости
  header.writeUInt16LE(32, entry + 6); // бит на пиксель
  header.writeUInt32LE(images[i].length, entry + 8);
  header.writeUInt32LE(offset, entry + 12);
  offset += images[i].length;
});
writeFileSync(new URL("icon.ico", dir), Buffer.concat([header, ...images]));

await browser.close();
console.log("Готово: app/icons/*.png, icon.ico");
