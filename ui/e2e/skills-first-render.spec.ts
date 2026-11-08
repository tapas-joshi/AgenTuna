import type { Page } from "@playwright/test";
import { expect, mockSkillsScale, test } from "./support/tauri-mock";

/**
 * TF-035 regression guard: the Skills screen must show its first row quickly
 * even with a 3,000-row snapshot, and finish deriving the full list shortly
 * after. The bound is generous (headless CI machines are slow) — the point is
 * to catch a regression back to a multi-second one-pass mount, not to measure
 * the 150 ms product budget; the GUI lane measures that on real hardware.
 */

/** Headless ceiling for the first painted row, from the route click. */
const FIRST_ROW_VISIBLE_LIMIT_MS = 1000;
/** Headless ceiling for the whole snapshot being derived and published. */
const FULL_LIST_LIMIT_MS = 2500;

interface FirstRenderProbe {
  click: number | null;
  firstRowVisible: number | null;
  fullList: number | null;
}

/** Polls every animation frame until the first row has a painted, on-screen box. */
async function installFirstRenderProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    // The probe slot is ours: this spec writes and reads it in the same file.
    const host = window as typeof window & { __firstRenderProbe: FirstRenderProbe };
    const probe: FirstRenderProbe = { click: null, firstRowVisible: null, fullList: null };
    host.__firstRenderProbe = probe;
    const rowVisible = (): boolean => {
      const row = document.querySelector<HTMLElement>('[role="row"][data-row-index="0"]');
      if (row === null) return false;
      const rect = row.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return false;
      const viewportHeight = window.innerHeight || document.documentElement.clientHeight;
      return rect.top < viewportHeight && rect.bottom > 0;
    };
    const check = (): void => {
      if (probe.firstRowVisible === null && rowVisible()) probe.firstRowVisible = performance.now();
      if (probe.fullList === null) {
        const table = document.querySelector<HTMLElement>('[role="table"][aria-label="Skills"]');
        if (table !== null && table.getAttribute("data-total-rows") === "3000") {
          probe.fullList = performance.now();
        }
      }
      if (probe.firstRowVisible !== null && probe.fullList !== null) return;
      requestAnimationFrame(check);
    };
    requestAnimationFrame(check);
  });
}

async function readProbe(page: Page): Promise<FirstRenderProbe> {
  return page.evaluate(() => {
    // Same slot installFirstRenderProbe created; see the type there.
    const host = window as typeof window & { __firstRenderProbe: FirstRenderProbe };
    return host.__firstRenderProbe;
  });
}

test("3,000-row Skills first row paints inside the regression bound", async ({ page }) => {
  await mockSkillsScale(page, 3000);
  await page.goto("/");
  const shell = page.getByRole("button", { name: "Skills", exact: true });
  await expect(shell).toBeVisible();

  await installFirstRenderProbe(page);
  await page.evaluate(() => {
    // Same slot installFirstRenderProbe created; see the type there.
    const host = window as typeof window & { __firstRenderProbe: FirstRenderProbe };
    host.__firstRenderProbe.click = performance.now();
  });
  await shell.click();

  await expect
    .poll(() => readProbe(page), { timeout: 15_000 })
    .toMatchObject({ firstRowVisible: expect.any(Number), fullList: expect.any(Number) });
  const probe = await readProbe(page);
  const firstRowMs = (probe.firstRowVisible ?? 0) - (probe.click ?? 0);
  const fullListMs = (probe.fullList ?? 0) - (probe.click ?? 0);
  console.log(`skills-first-render: firstRowVisibleMs=${firstRowMs.toFixed(0)} fullListMs=${fullListMs.toFixed(0)}`);
  test.info().annotations.push({
    type: "measurement",
    description: `firstRowVisibleMs=${firstRowMs.toFixed(0)} fullListMs=${fullListMs.toFixed(0)}`,
  });

  expect(firstRowMs, JSON.stringify(probe)).toBeGreaterThanOrEqual(0);
  expect(firstRowMs, JSON.stringify(probe)).toBeLessThan(FIRST_ROW_VISIBLE_LIMIT_MS);
  expect(fullListMs, JSON.stringify(probe)).toBeLessThan(FULL_LIST_LIMIT_MS);
});
