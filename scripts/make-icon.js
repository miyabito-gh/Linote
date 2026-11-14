// res/icon/*.svg から res/linote.ico を作る（手動で実行。ビルドには不要）。
// 48 px 以上は linote-large.svg（カーソルあり）、32 px 以下は linote-small.svg（2 行）。
// 使い方: node scripts/make-icon.js   （Playwright と Chromium が必要）
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const root = path.join(__dirname, '..');
const sizes = [256, 128, 64, 48, 32, 24, 20, 16];

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  const pngs = [];
  for (const size of sizes) {
    const file = size >= 48 ? 'linote-large.svg' : 'linote-small.svg';
    const svg = fs.readFileSync(path.join(root, 'res/icon', file), 'utf8');
    await page.setViewportSize({ width: size, height: size });
    await page.setContent(
      `<html><body style="margin:0;background:transparent">` +
        `<img src="data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}" ` +
        `width="${size}" height="${size}" style="display:block"></body></html>`
    );
    pngs.push(await page.screenshot({ omitBackground: true }));
  }
  await browser.close();
  // ICO: ヘッダー 6 バイト + 項目 16 バイト × n + PNG の本体（Windows Vista 以降は PNG を読める）。
  const header = Buffer.alloc(6 + 16 * sizes.length);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  sizes.forEach((size, i) => {
    const e = 6 + 16 * i;
    header.writeUInt8(size >= 256 ? 0 : size, e);
    header.writeUInt8(size >= 256 ? 0 : size, e + 1);
    header.writeUInt8(0, e + 2);
    header.writeUInt8(0, e + 3);
    header.writeUInt16LE(1, e + 4);
    header.writeUInt16LE(32, e + 6);
    header.writeUInt32LE(pngs[i].length, e + 8);
    header.writeUInt32LE(offset, e + 12);
    offset += pngs[i].length;
  });
  fs.writeFileSync(path.join(root, 'res/linote.ico'), Buffer.concat([header, ...pngs]));
  console.log('res/linote.ico', offset, 'bytes');
})();
