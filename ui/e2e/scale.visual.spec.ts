import { expect, test } from "./support/tauri-mock";

/**
 * The number in a numeric Tailwind utility is the pixel value. Tailwind's own
 * base is 0.25rem per step, which silently rendered `gap-6` as 24px, `size-20`
 * as 80px and `rounded-8` as no radius at all. `theme/scale.test.ts` pins the
 * token table; this spec measures the CSS the browser actually applies, so a
 * regression in the theme or the Tailwind wiring fails here too.
 *
 * The classes below are listed literally so Tailwind emits them even though no
 * screen uses every step.
 */
const PROBES = [
  { cls: "gap-6", prop: "gap", px: "6px" },
  { cls: "gap-4", prop: "gap", px: "4px" },
  { cls: "gap-24", prop: "gap", px: "24px" },
  { cls: "size-20", prop: "width", px: "20px" },
  { cls: "size-20", prop: "height", px: "20px" },
  { cls: "h-40", prop: "height", px: "40px" },
  { cls: "w-256", prop: "width", px: "256px" },
  { cls: "max-h-160", prop: "maxHeight", px: "160px" },
  { cls: "p-1", prop: "paddingTop", px: "1px" },
  { cls: "p-16", prop: "paddingTop", px: "16px" },
  { cls: "px-14", prop: "paddingLeft", px: "14px" },
  { cls: "rounded-4", prop: "borderTopLeftRadius", px: "4px" },
  { cls: "rounded-8", prop: "borderTopLeftRadius", px: "8px" },
  { cls: "rounded-12", prop: "borderTopLeftRadius", px: "12px" },
  { cls: "text-13", prop: "fontSize", px: "13px" },
] as const;

test("numeric utilities resolve to their pixel value", async ({ page }) => {
  await page.goto("/");

  const measured = await page.evaluate((probes) => {
    const host = document.createElement("div");
    host.style.cssText = "position:fixed;inset-block-start:0;inset-inline-start:0;width:800px";
    document.body.append(host);
    const values = probes.map(({ cls, prop }) => {
      const probe = document.createElement("div");
      probe.className = cls;
      probe.style.display = "flex";
      host.append(probe);
      const value = getComputedStyle(probe).getPropertyValue(
        prop.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`),
      );
      probe.remove();
      return value;
    });
    host.remove();
    return values;
  }, PROBES.map(({ cls, prop }) => ({ cls, prop })));

  for (const [index, probe] of PROBES.entries()) {
    expect(measured[index], `${probe.cls} should compute ${probe.px}`).toBe(probe.px);
  }
});
