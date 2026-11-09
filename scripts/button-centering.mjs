import { chromium } from "../ui/node_modules/@playwright/test/index.mjs";
import { preview } from "../ui/node_modules/vite/dist/node/index.js";

const root = new URL("../ui", import.meta.url).pathname;
const server = await preview({
  configFile: `${root}/vite.config.ts`,
  root,
  preview: { host: "127.0.0.1", port: 4173, strictPort: true },
});
const variants = {
  primary: "border border-accent-fill bg-accent-fill text-on-accent",
  secondary: "border border-border bg-raised text-text",
  ghost: "border border-transparent bg-transparent text-text-secondary",
  destructive: "border border-danger/60 bg-transparent text-danger",
};
const sizes = {
  sm: "h-24 gap-4 px-8 text-11",
  md: "h-32 gap-8 px-12 text-13",
  lg: "h-32 gap-8 px-16 text-14",
};
const expectedHeights = { sm: 24, md: 32, lg: 32 };
const base =
  "inline-flex shrink-0 items-center justify-center rounded-6 optical-label-center font-medium transition-colors";
const limit = (scale) => (scale === 1 ? 0.5 : 1);
let browser;

try {
  browser = await chromium.launch({ headless: true });
  for (const scale of [1, 2]) {
    const context = await browser.newContext({ viewport: { width: 980, height: 900 }, deviceScaleFactor: scale });
    const page = await context.newPage();
    await page.goto("http://127.0.0.1:4173/");
    await page.evaluate(() => {
      document.body.innerHTML = '<main id="button-centering" style="display:grid;grid-template-columns:repeat(4,max-content);gap:12px;padding:24px;background:var(--at-color-surface)"></main>';
    });
    for (const font of ["font-ui", "font-mono"]) {
      for (const [size, sizeClasses] of Object.entries(sizes)) {
        for (const [variant, variantClasses] of Object.entries(variants)) {
          const id = `${font}-${size}-${variant}`;
          await page.evaluate(({ id, font, sizeClasses, variantClasses, base }) => {
            const button = document.createElement("button");
            button.id = id;
            button.textContent = "Add to repo";
            button.className = `${base} ${variantClasses} ${sizeClasses} ${font}`;
            document.querySelector("#button-centering").append(button);
          }, { id, font, sizeClasses, variantClasses, base });
          const button = page.locator(`#${id}`);
          await page.evaluate(async (id) => {
            const element = document.getElementById(id);
            await document.fonts.load(getComputedStyle(element).font, element.textContent);
          }, id);
          const image = await button.screenshot();
          const measured = await page.evaluate(async (bytes) => {
            const bitmap = await createImageBitmap(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
            const canvas = document.createElement("canvas");
            canvas.width = bitmap.width;
            canvas.height = bitmap.height;
            const context = canvas.getContext("2d", { willReadFrequently: true });
            context.drawImage(bitmap, 0, 0);
            const { data, width, height } = context.getImageData(0, 0, bitmap.width, bitmap.height);
            const pixel = (x, y) => {
              const index = (y * width + x) * 4;
              return [data[index], data[index + 1], data[index + 2]];
            };
            const background = pixel(Math.min(5, width - 1), Math.floor(height / 2));
            let first = height;
            let last = -1;
            for (let y = 2; y < height - 2; y += 1) {
              let rowHasInk = false;
              for (let x = Math.floor(width * 0.12); x < Math.ceil(width * 0.88); x += 1) {
                const rgb = pixel(x, y);
                // Keep antialiased red destructive ink; a 70-point cutoff clipped its outer rows.
                if (Math.hypot(rgb[0] - background[0], rgb[1] - background[1], rgb[2] - background[2]) > 20) {
                  rowHasInk = true;
                  break;
                }
              }
              if (rowHasInk) {
                first = Math.min(first, y);
                last = y;
              }
            }
            return { height, first, last, hasInk: last >= first, offset: (first + last + 1) / 2 - height / 2 };
          }, [...image]);
          if (!measured.hasInk) {
            throw new Error(`${id} screenshot contained no measurable label ink`);
          }
          if (measured.height !== expectedHeights[size] * scale) {
            throw new Error(`${id} height ${measured.height / scale}px changed from ${expectedHeights[size]}px`);
          }
          const offsetCss = measured.offset / scale;
          console.log(`${scale}x ${font} ${size}/${variant}: ${offsetCss.toFixed(2)} CSS px`);
          if (Math.abs(measured.offset) > limit(scale)) {
            throw new Error(`${id} ink center offset ${measured.offset}px exceeds ${limit(scale)} device px at ${scale}x`);
          }
        }
      }
    }
    await context.close();
  }
} finally {
  await browser?.close();
  await new Promise((resolve, reject) => server.httpServer.close((error) => error ? reject(error) : resolve()));
}
