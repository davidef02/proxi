// Regenerates icon.png (512) and icon.ico (PNG-in-ICO) from icon.svg. Run: node_modules\electron\dist\electron.exe app/desktop/make-icon.mjs
import { app, BrowserWindow, nativeImage } from "electron";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const SIZES = [16, 24, 32, 48, 64, 128, 256];

// No top-level await: Electron waits for the ESM main to finish loading before it fires `ready`.
async function run() {
await app.whenReady();
const win = new BrowserWindow({ width: 512, height: 512, show: false, useContentSize: true, transparent: true, frame: false });
const svg = fs.readFileSync(path.join(here, "icon.svg"), "utf8");
await win.loadURL("data:text/html;charset=utf-8," + encodeURIComponent(`<body style="margin:0;background:transparent">${svg}</body>`));
await new Promise((r) => setTimeout(r, 300));
const big = await win.webContents.capturePage({ x: 0, y: 0, width: 512, height: 512 });
fs.writeFileSync(path.join(here, "icon.png"), big.toPNG());

const pngs = SIZES.map((s) => nativeImage.createFromBuffer(big.toPNG()).resize({ width: s, height: s, quality: "best" }).toPNG());
const head = Buffer.alloc(6 + 16 * pngs.length);
head.writeUInt16LE(1, 2);
head.writeUInt16LE(pngs.length, 4);
let offset = head.length;
pngs.forEach((p, i) => {
  const s = SIZES[i], o = 6 + 16 * i;
  head[o] = s >= 256 ? 0 : s; head[o + 1] = s >= 256 ? 0 : s;
  head.writeUInt16LE(1, o + 4); head.writeUInt16LE(32, o + 6);
  head.writeUInt32LE(p.length, o + 8); head.writeUInt32LE(offset, o + 12);
  offset += p.length;
});
fs.writeFileSync(path.join(here, "icon.ico"), Buffer.concat([head, ...pngs]));
app.quit();
}
run().catch((e) => { console.error(e); app.exit(1); });
