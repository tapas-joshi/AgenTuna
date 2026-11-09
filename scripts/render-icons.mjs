#!/usr/bin/env node
// Renders crates/at-app/icons/source/agentuna-icon.svg into every raster the app ships:
//   icon.icns   macOS (Apple template artwork: margin + soft shadow; needs `iconutil`, macOS only)
//   icon.ico    Windows (16-256 px, PNG-compressed entries, tile fills the canvas)
//   icon.png, 128x128.png, 128x128@2x.png, 32x32.png   Linux / Tauri
//
// Usage: node scripts/render-icons.mjs [--check]
//   --check  render to a temp dir and exit non-zero when a committed file would change
//
// Rendering uses the Chromium that Playwright already installed for the UI tests, so no extra
// tool is needed. Run `pnpm --dir ui install` first.
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const iconDir = path.join(root, "crates/at-app/icons");
const source = readFileSync(path.join(iconDir, "source/agentuna-icon.svg"), "utf8");
const require = createRequire(path.join(root, "ui/package.json"));
const { chromium } = require("@playwright/test");

const check = process.argv.includes("--check");
const outDir = check ? mkdtempSync(path.join(tmpdir(), "agentuna-icons-")) : iconDir;

// The flat variant drops the template margin and shadow: the tile fills the whole canvas.
const flat = source
  .replace('viewBox="0 0 1024 1024"', 'viewBox="100 100 824 824"')
  .replace(' filter="url(#tile-shadow)"', "");

async function render(browser, svg, size) {
  const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
  const body = svg.replace(/width="1024" height="1024"/, `width="${size}" height="${size}"`);
  await page.setContent(`<!doctype html><style>html,body{margin:0;background:transparent}svg{display:block}</style>${body}`);
  const png = await page.screenshot({ omitBackground: true, clip: { x: 0, y: 0, width: size, height: size } });
  await page.close();
  return png;
}

function ico(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  let offset = 6 + entries.length * 16;
  const dir = [];
  for (const { size, png } of entries) {
    const entry = Buffer.alloc(16);
    entry.writeUInt8(size >= 256 ? 0 : size, 0);
    entry.writeUInt8(size >= 256 ? 0 : size, 1);
    entry.writeUInt16LE(1, 4);
    entry.writeUInt16LE(32, 6);
    entry.writeUInt32LE(png.length, 8);
    entry.writeUInt32LE(offset, 12);
    offset += png.length;
    dir.push(entry);
  }
  return Buffer.concat([header, ...dir, ...entries.map((entry) => entry.png)]);
}

const browser = await chromium.launch();
try {
  const flatPng = async (size) => ({ size, png: await render(browser, flat, size) });
  writeFileSync(path.join(outDir, "icon.png"), (await flatPng(512)).png);
  writeFileSync(path.join(outDir, "128x128.png"), (await flatPng(128)).png);
  writeFileSync(path.join(outDir, "128x128@2x.png"), (await flatPng(256)).png);
  writeFileSync(path.join(outDir, "32x32.png"), (await flatPng(32)).png);
  const entries = [];
  for (const size of [16, 32, 48, 64, 128, 256]) entries.push(await flatPng(size));
  writeFileSync(path.join(outDir, "icon.ico"), ico(entries));

  if (process.platform === "darwin") {
    const set = path.join(mkdtempSync(path.join(tmpdir(), "agentuna-iconset-")), "AppIcon.iconset");
    mkdirSync(set);
    const names = [
      [16, "icon_16x16.png"], [32, "icon_16x16@2x.png"], [32, "icon_32x32.png"], [64, "icon_32x32@2x.png"],
      [128, "icon_128x128.png"], [256, "icon_128x128@2x.png"], [256, "icon_256x256.png"],
      [512, "icon_256x256@2x.png"], [512, "icon_512x512.png"], [1024, "icon_512x512@2x.png"],
    ];
    for (const [size, name] of names) writeFileSync(path.join(set, name), await render(browser, source, size));
    execFileSync("iconutil", ["-c", "icns", set, "-o", path.join(outDir, "icon.icns")]);
    rmSync(path.dirname(set), { recursive: true, force: true });
  } else {
    console.error("skipping icon.icns: iconutil only exists on macOS");
  }
} finally {
  await browser.close();
}

if (check) {
  const differing = [];
  for (const name of ["icon.png", "128x128.png", "128x128@2x.png", "32x32.png", "icon.ico", "icon.icns"]) {
    try {
      if (!readFileSync(path.join(outDir, name)).equals(readFileSync(path.join(iconDir, name)))) differing.push(name);
    } catch {
      differing.push(name);
    }
  }
  rmSync(outDir, { recursive: true, force: true });
  if (differing.length > 0) {
    console.error(`icons are stale, run node scripts/render-icons.mjs: ${differing.join(", ")}`);
    process.exit(1);
  }
  console.log("icons are up to date");
} else {
  console.log(`wrote icons to ${path.relative(root, outDir)}`);
}
