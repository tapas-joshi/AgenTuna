import type { Page } from "@playwright/test";
import { expect, mockSkillsScale, test } from "./support/tauri-mock";

/**
 * TF-035 regression guard, measured in a real browser.
 *
 * The release app mounted 188 rows for one 600 px viewport because the
 * virtualizer measured the scroll container's *content* height instead of the
 * height it can show. These checks fail if the mounted row window grows with
 * the data set again: the row count and the DOM node count must depend on the
 * viewport, never on how many rows the snapshot contains.
 */

/** Rows a 600 px viewport can show plus one overscan page. Hard ceiling. */
const MOUNTED_ROW_LIMIT = 40;
/** Generous wall-clock ceiling for the first painted rows (budget is 150 ms). */
const FIRST_PAINT_LIMIT_MS = 1500;

interface ScaleProbe {
  click: number | null;
  firstRow: number | null;
}

interface TableMeasure {
  readonly mountedRows: number;
  readonly renderedRows: number;
  readonly totalRows: number;
  /** The scroll container's own height: the number the virtualizer measures. */
  readonly scrollHeight: number;
  /** Window height: the ceiling the table box is allowed to reach. */
  readonly screenHeight: number;
  readonly domNodes: number;
  readonly firstPaintMs: number | null;
}

/** Records the moment the first row reaches the DOM, relative to the click. */
async function installProbe(page: Page): Promise<void> {
  await page.evaluate(() => {
    // The probe slot is ours: installProbe writes it, the measurement reads it.
    const host = window as unknown as { __scaleProbe: ScaleProbe };
    const probe: ScaleProbe = { click: null, firstRow: null };
    host.__scaleProbe = probe;
    const observer = new MutationObserver(() => {
      const table = document.querySelector('[role="table"][aria-label="Skills"]');
      const rendered = table === null ? 0 : Number(table.getAttribute("data-rendered-rows") ?? "0");
      if (rendered > 0 && probe.firstRow === null) {
        probe.firstRow = performance.now();
        observer.disconnect();
      }
    });
    observer.observe(document.body, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["data-rendered-rows"],
    });
  });
}


/**
 * Makes the Skills table report a `height` px viewport without changing layout.
 *
 * This reproduces the release build's bad measurement inside a real browser:
 * WebKit handed the virtualizer a client height of roughly 12,000 px for a
 * 600 px prop, and the virtualizer mounted a row for every rowHeight slice of
 * it. Chromium resolves the same flex layout correctly, so the measurement has
 * to be forced here to keep the guard honest in both engines.
 */
async function reportTallViewport(page: Page, height: number): Promise<void> {
  await page.addInitScript((forced: number) => {
    const original = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientHeight");
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get(this: HTMLElement) {
        if (this.getAttribute("role") === "table") return forced;
        return original?.get?.call(this) ?? 0;
      },
    });
  }, height);
}

async function measureSkills(page: Page, rows: number): Promise<TableMeasure> {
  await installProbe(page);
  await page.evaluate(() => {
    // Same probe slot installProbe created; see the type there.
    const host = window as unknown as { __scaleProbe: ScaleProbe };
    host.__scaleProbe.click = performance.now();
  });
  await page.getByRole("button", { name: "Skills", exact: true }).click();

  const table = page.locator('[role="table"][aria-label="Skills"]');
  await expect(table).toHaveAttribute("data-total-rows", String(rows));
  await expect(table).toHaveAttribute("data-rendered-rows", /\d+/);

  const measured = await page.evaluate(() => {
    const element = document.querySelector('[role="table"][aria-label="Skills"]');
    // Same probe slot installProbe created; see the type there.
    const probe = (window as unknown as { __scaleProbe: ScaleProbe }).__scaleProbe;
    const rendered = element === null ? 0 : Number(element.getAttribute("data-rendered-rows") ?? "0");
    return {
      mountedRows: document.querySelectorAll('[role="row"][data-row-index]').length,
      renderedRows: rendered,
      totalRows: element === null ? 0 : Number(element.getAttribute("data-total-rows") ?? "0"),
      scrollHeight: element instanceof HTMLElement ? element.clientHeight : 0,
      screenHeight: window.innerHeight,
      domNodes: document.querySelectorAll("*").length,
      firstPaintMs: probe.firstRow === null || probe.click === null ? null : probe.firstRow - probe.click,
    };
  });
  return measured;
}

test("3,000-row Skills list mounts only the viewport's rows and paints fast", async ({ page }) => {
  await mockSkillsScale(page, 3000);
  await page.goto("/");
  const measured = await measureSkills(page, 3000);

  expect(measured.totalRows, JSON.stringify(measured)).toBe(3000);
  expect(measured.mountedRows, JSON.stringify(measured)).toBe(measured.renderedRows);
  expect(measured.mountedRows, JSON.stringify(measured)).toBeLessThanOrEqual(MOUNTED_ROW_LIMIT);
  // The box stays layout-driven up to one screen; it may never grow with the
  // spacer, which is what fed the virtualizer a 15,000 px measurement.
  expect(measured.scrollHeight, JSON.stringify(measured)).toBeLessThanOrEqual(
    Math.max(640, measured.screenHeight),
  );
  expect(measured.domNodes, JSON.stringify(measured)).toBeLessThan(1500);
  expect(measured.firstPaintMs, JSON.stringify(measured)).not.toBeNull();
  expect(measured.firstPaintMs ?? Infinity, JSON.stringify(measured)).toBeLessThan(FIRST_PAINT_LIMIT_MS);
});

test("the mounted row window does not grow with the snapshot", async ({ page }) => {
  await mockSkillsScale(page, 300);
  await page.goto("/");
  const small = await measureSkills(page, 300);

  await mockSkillsScale(page, 3000);
  await page.goto("/");
  const large = await measureSkills(page, 3000);

  expect(small.totalRows, JSON.stringify(small)).toBe(300);
  expect(large.totalRows, JSON.stringify(large)).toBe(3000);
  expect(large.mountedRows, JSON.stringify({ small, large })).toBe(small.mountedRows);
  expect(large.domNodes - small.domNodes, JSON.stringify({ small, large })).toBeLessThan(300);
});

test("row elements keep stable ids across re-sorts and filter changes", async ({ page }) => {
  await mockSkillsScale(page, 3000);
  await page.goto("/");
  await measureSkills(page, 3000);

  const table = page.locator('[role="table"][aria-label="Skills"]');
  const before = await table.locator("[data-row-id]").evaluateAll((rows) =>
    rows.map((row) => row.getAttribute("data-row-id")),
  );
  expect(new Set(before).size).toBe(before.length);

  // Sorting reorders the model; the same ids must still identify the same rows.
  await page.getByRole("button", { name: "Skill", exact: true }).click();
  const after = await table.locator("[data-row-id]").evaluateAll((rows) =>
    rows.map((row) => row.getAttribute("data-row-id")),
  );
  expect(after.length).toBeGreaterThan(0);
  expect(after.length, JSON.stringify({ before: before.slice(0, 3), after: after.slice(0, 3) })).toBeLessThanOrEqual(
    MOUNTED_ROW_LIMIT,
  );
  expect(after.some((id) => id !== null && before.includes(id))).toBe(true);
});

test("MCP Servers list uses the same viewport clamp", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  const table = page.locator('[role="table"][aria-label="MCP Servers"]');
  await expect(table).toBeVisible();

  const measured = await page.evaluate(() => {
    const element = document.querySelector('[role="table"][aria-label="MCP Servers"]');
    return {
      mountedRows: document.querySelectorAll('[role="row"][data-row-index]').length,
      renderedRows: element === null ? 0 : Number(element.getAttribute("data-rendered-rows") ?? "0"),
      scrollHeight: element instanceof HTMLElement ? element.clientHeight : 0,
    };
  });

  expect(measured.mountedRows, JSON.stringify(measured)).toBe(measured.renderedRows);
  expect(measured.mountedRows, JSON.stringify(measured)).toBeLessThanOrEqual(MOUNTED_ROW_LIMIT);
  expect(measured.scrollHeight, JSON.stringify(measured)).toBeLessThanOrEqual(640);
});

test("a 12,000 px viewport measurement still mounts one screenful", async ({ page }) => {
  await mockSkillsScale(page, 3000);
  await reportTallViewport(page, 12_000);
  await page.goto("/");
  const measured = await measureSkills(page, 3000);

  expect(measured.totalRows, JSON.stringify(measured)).toBe(3000);
  expect(measured.mountedRows, JSON.stringify(measured)).toBe(measured.renderedRows);
  expect(measured.mountedRows, JSON.stringify(measured)).toBeLessThanOrEqual(MOUNTED_ROW_LIMIT);
  expect(measured.firstPaintMs ?? Infinity, JSON.stringify(measured)).toBeLessThan(FIRST_PAINT_LIMIT_MS);
});
