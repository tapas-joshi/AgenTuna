import { expect, test } from "./support/tauri-mock";
import type { Page } from "@playwright/test";

/**
 * TU-024: HiDPI and font rendering.
 *
 * These assertions complement the token-level vitest guards in
 * `ui/src/theme/typography.test.tsx` and the manual zoom walk in
 * `ui/src/theme/MANUAL-CHECKS.md`. Real layout is required, so this spec runs
 * in Chromium: the config's 1x and 2x projects, plus a dedicated 1.5x context.
 * Screenshot baselines are intentionally not asserted here; the visual-triage
 * lane owns `ui/e2e/snapshots`.
 */

type DensityBucket = "1x" | "1.5x" | "2x";

function bucketFor(ratio: number): DensityBucket {
  if (ratio >= 1.75) return "2x";
  if (ratio >= 1.25) return "1.5x";
  return "1x";
}

async function openSkills(page: Page): Promise<void> {
  await page.goto("/");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await expect(page.getByRole("main", { name: "Skills" })).toBeVisible();
}

/**
 * Returns visible elements whose own box clips its content. Intentional
 * ellipsis truncation is excluded: the task is to catch glyphs cut off by a
 * fixed height or a hidden overflow, not single-line table labels that the
 * design marks with an ellipsis.
 */
async function clippedLabels(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const clipped: string[] = [];
    const selectors = [
      "button",
      "[role='tab']",
      "[role='columnheader']",
      "[role='cell']",
      "label",
      "h1",
      "h2",
      "h3",
    ].join(",");
    for (const element of document.querySelectorAll<HTMLElement>(selectors)) {
      const style = getComputedStyle(element);
      if (style.display === "none" || style.visibility === "hidden") continue;
      if (element.getClientRects().length === 0) continue;
      // Screen-reader-only labels are clipped on purpose; they have no glyphs on screen.
      const box = element.getBoundingClientRect();
      if (box.width <= 1 || box.height <= 1) continue;
      const clipsX = style.overflowX === "hidden" || style.overflowX === "clip";
      const clipsY = style.overflowY === "hidden" || style.overflowY === "clip";
      const ellipsis = style.textOverflow === "ellipsis";
      const horizontal = element.scrollWidth - element.clientWidth;
      const vertical = element.scrollHeight - element.clientHeight;
      const isClippedX = clipsX && horizontal > 1 && !ellipsis;
      const isClippedY = clipsY && vertical > 1;
      if (isClippedX || isClippedY) {
        clipped.push(
          `${element.tagName.toLowerCase()} "${(element.textContent ?? "").trim().slice(0, 48)}" (dx ${horizontal}, dy ${vertical})`,
        );
      }
    }
    return clipped;
  });
}

/** Column headers must not overlap their neighbours at any device ratio. */
async function overlappingHeaders(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const headers = [...document.querySelectorAll<HTMLElement>("[role='columnheader']")].filter(
      (element) => element.getClientRects().length > 0,
    );
    const overlaps: string[] = [];
    for (let left = 0; left < headers.length; left += 1) {
      for (let right = left + 1; right < headers.length; right += 1) {
        const a = headers[left]?.getBoundingClientRect();
        const b = headers[right]?.getBoundingClientRect();
        if (a === undefined || b === undefined) continue;
        const separated = a.right <= b.left + 0.5 || b.right <= a.left + 0.5;
        if (!separated) {
          overlaps.push(`${headers[left]?.textContent ?? "?"} / ${headers[right]?.textContent ?? "?"}`);
        }
      }
    }
    return overlaps;
  });
}

test("the device pixel ratio drives the density bucket and hairline token", async ({ page }) => {
  await page.goto("/");
  const state = await page.evaluate(() => {
    const root = document.documentElement;
    const style = getComputedStyle(root);
    return {
      ratio: window.devicePixelRatio,
      bucket: root.getAttribute("data-density"),
      pixelRatio: style.getPropertyValue("--at-pixel-ratio").trim(),
      hairline: style.getPropertyValue("--at-hairline").trim(),
    };
  });
  expect(state.bucket).toBe(bucketFor(state.ratio));
  expect(Number(state.pixelRatio)).toBe(state.ratio);
  expect(state.hairline).not.toBe("");
});

test("bundled fonts load and monospace advances are uniform", async ({ page }) => {
  await page.goto("/");
  const metrics = await page.evaluate(async () => {
    await document.fonts.ready;
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("2d");
    if (context === null) return null;
    context.font = '13px "JetBrains Mono", "Menlo", ui-monospace, "SFMono-Regular", monospace';
    return {
      interLoaded: document.fonts.check('14px "Inter"'),
      monoLoaded: document.fonts.check('13px "JetBrains Mono"'),
      narrow: context.measureText("iiii").width,
      wide: context.measureText("MMMM").width,
    };
  });
  expect(metrics, "canvas 2d is available").not.toBeNull();
  expect(metrics?.interLoaded, "Inter is served from the bundle").toBe(true);
  expect(metrics?.monoLoaded, "JetBrains Mono is served from the bundle").toBe(true);
  expect(metrics?.narrow).toBeCloseTo(metrics?.wide ?? Number.NaN, 5);

  await page.getByRole("button", { name: "Instructions", exact: true }).click();
  await expect(page.getByRole("main", { name: "Instructions" })).toBeVisible();
  const scroller = await page.evaluate(() => {
    const node = document.querySelector(".cm-scroller");
    if (node === null) return null;
    const style = getComputedStyle(node);
    return { family: style.fontFamily, size: style.fontSize, lineHeight: style.lineHeight };
  });
  expect(scroller, "the instruction editor renders its scroller").not.toBeNull();
  expect(scroller?.family).toContain("JetBrains Mono");
  expect(Number.parseFloat(scroller?.lineHeight ?? "0")).toBeGreaterThan(
    Number.parseFloat(scroller?.size ?? "0"),
  );
});

test("table counts use tabular figures and no label is clipped at this ratio", async ({ page }) => {
  await openSkills(page);
  const state = await page.evaluate(() => {
    const count = document.querySelector<HTMLElement>("[data-slot='chip'] .tabular-nums");
    return {
      countVariant: count === null ? null : getComputedStyle(count).fontVariantNumeric,
      pageOverflow:
        document.documentElement.scrollWidth - document.documentElement.clientWidth,
    };
  });
  expect(state.countVariant, "the filter chip renders a numeric count").toContain("tabular-nums");
  expect(state.pageOverflow, "the layout stays inside the viewport").toBeLessThanOrEqual(0);
  expect(await clippedLabels(page)).toEqual([]);
  expect(await overlappingHeaders(page)).toEqual([]);
});

test.describe("1.5x display scaling", () => {
  test.use({ deviceScaleFactor: 1.5 });

  test("applies the 1.5x bucket and keeps Skills and Instructions unclipped", async ({ page }) => {
    await openSkills(page);
    const state = await page.evaluate(() => ({
      ratio: window.devicePixelRatio,
      bucket: document.documentElement.getAttribute("data-density"),
      pageOverflow:
        document.documentElement.scrollWidth - document.documentElement.clientWidth,
    }));
    expect(state.ratio).toBe(1.5);
    expect(state.bucket).toBe("1.5x");
    expect(state.pageOverflow, "the layout stays inside the viewport").toBeLessThanOrEqual(0);
    expect(await clippedLabels(page), "Skills labels are unclipped at 1.5x").toEqual([]);
    expect(await overlappingHeaders(page), "Skills columns do not overlap at 1.5x").toEqual([]);

    await page.getByRole("button", { name: "Instructions", exact: true }).click();
    await expect(page.getByRole("main", { name: "Instructions" })).toBeVisible();
    expect(await clippedLabels(page), "Instructions labels are unclipped at 1.5x").toEqual([]);
  });
});
