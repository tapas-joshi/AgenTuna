import type { Locator, Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

const sizes = [
  { width: 960, height: 640 },
  { width: 1180, height: 720 },
  { width: 1280, height: 800 },
] as const;

type Screen = {
  readonly name: string;
  readonly open: (page: Page) => Promise<void>;
  readonly ready: (page: Page) => Locator;
  readonly action: (page: Page) => Locator;
  readonly baseline?: string;
  /** A list: every column must fit the window instead of scrolling sideways. */
  readonly fitsWithoutSideScroll?: boolean;
};

const navigation = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Workspace" }).getByRole("button", { name, exact: true });

async function openMcpDetail(page: Page) {
  await navigation(page, "MCP Servers").click();
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();
  await expect(page.getByRole("main", { name: "MCP Server Detail" }).getByRole("heading", { name: "Visual server" })).toBeVisible();
}

const screens: readonly Screen[] = [
  {
    name: "overview",
    open: async () => {},
    ready: (page) => page.getByRole("main", { name: "Overview" }).getByRole("button", { name: /\+ Add to/ }),
    action: (page) => page.getByRole("main", { name: "Overview" }).getByRole("button", { name: /\+ Add to/ }),
  },
  {
    name: "skills-list",
    open: async (page) => { await navigation(page, "Skills").click(); },
    // The toggle renders only after the async agents list has loaded, so it is the readiness signal for
    // a stable screenshot (waiting for a row alone raced it).
    ready: (page) => page.getByRole("main", { name: "Skills" }).getByRole("button", { name: /Show all agents/ }),
    action: (page) => page.getByRole("main", { name: "Skills" }).getByRole("button", { name: "+ New skill" }).first(),
    baseline: "small-skills",
    fitsWithoutSideScroll: true,
  },
  {
    name: "mcp-list",
    open: async (page) => { await navigation(page, "MCP Servers").click(); },
    ready: (page) => page.getByRole("main", { name: "MCP Servers" }).getByText("Visual server", { exact: true }),
    action: (page) => page.getByRole("main", { name: "MCP Servers" }).getByRole("button", { name: "+ Add server" }).first(),
    fitsWithoutSideScroll: true,
  },
  {
    name: "effective-config",
    open: async (page) => { await navigation(page, "Effective config").click(); },
    ready: (page) => page.getByRole("region", { name: "Effective item detail" }).getByText("Why is this on?", { exact: true }).first(),
    action: (page) => page.getByRole("region", { name: "Effective item detail" }).getByRole("button", { name: "Why is this on?" }).first(),
  },
  {
    name: "sync-and-drift",
    open: async (page) => { await navigation(page, "Sync & drift").click(); },
    ready: (page) => page.getByRole("main", { name: "Sync and drift" }).getByText("Code Review", { exact: true }).first(),
    action: (page) => page.getByRole("main", { name: "Sync and drift" }).getByRole("button", { name: "Pause watching" }),
  },
  {
    name: "instructions",
    open: async (page) => { await navigation(page, "Instructions").click(); },
    ready: (page) => page.getByRole("main", { name: "Instructions" }).getByRole("button", { name: "AGENTS.md", exact: true }),
    action: (page) => page.getByRole("main", { name: "Instructions" }).getByRole("button", { name: "New rule file" }).first(),
  },
  {
    name: "agents",
    open: async (page) => { await navigation(page, "Agents").click(); },
    ready: (page) => page.getByRole("main", { name: "Agents" }).getByText("Claude Code", { exact: true }).first(),
    action: (page) => page.getByRole("main", { name: "Agents" }).getByRole("button", { name: "Rescan all agents" }),
  },
  {
    name: "skill-detail",
    open: async (page) => {
      await page.evaluate(() => {
        (window as typeof window & { __AT_TEST_MOCKS__: { emit: (event: string, payload: unknown) => void } }).__AT_TEST_MOCKS__.emit("deep-link", "agentuna://item/itm_visual_skill");
      });
    },
    ready: (page) => page.getByRole("main", { name: "Skill detail" }).getByRole("heading", { name: "Code Review", exact: true }).first(),
    action: (page) => page.getByRole("main", { name: "Skill detail" }).getByRole("button", { name: "Edit", exact: true }),
  },
  {
    name: "mcp-detail",
    open: openMcpDetail,
    ready: (page) => page.getByRole("main", { name: "MCP Server Detail" }).getByRole("heading", { name: "Visual server" }),
    action: (page) => page.getByRole("main", { name: "MCP Server Detail" }).getByRole("button", { name: "Edit", exact: true }),
  },
  {
    name: "mcp-editor",
    open: async (page) => {
      await openMcpDetail(page);
      await page.getByRole("main", { name: "MCP Server Detail" }).getByRole("button", { name: "Edit", exact: true }).click();
    },
    ready: (page) => page.getByRole("main", { name: "Edit Visual server" }).getByRole("heading", { name: "Edit Visual server" }),
    action: (page) => page.getByRole("main", { name: "Edit Visual server" }).getByRole("button", { name: "Review edit" }),
    baseline: "small-mcp-editor",
  },
  {
    name: "add-skill-source",
    open: async (page) => {
      await page.getByRole("button", { name: "Add", exact: true }).click();
      await page.getByRole("menuitem", { name: "New skill" }).click();
    },
    ready: (page) => page.getByRole("heading", { name: "Add a skill", exact: true }),
    action: (page) => page.getByRole("main", { name: "Add a skill" }).getByRole("button", { name: "Continue", exact: true }),
  },
  {
    name: "migrate-scan",
    open: async (page) => {
      await page.keyboard.press("Meta+k");
      const palette = page.getByRole("dialog", { name: "Command palette" });
      await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
      await palette.getByRole("option", { name: /Import native items/ }).click();
    },
    ready: (page) => page.getByRole("main", { name: "Move into AgenTuna" }).getByText("Visual migration skill", { exact: true }),
    action: (page) => page.getByRole("main", { name: "Move into AgenTuna" }).getByRole("button", { name: "Continue with 0" }),
  },
  {
    name: "preferences",
    open: async (page) => { await page.keyboard.press("Meta+,"); },
    ready: (page) => page.getByRole("main", { name: "Preferences" }).getByRole("heading", { name: "Preferences" }),
    action: (page) => page.getByRole("main", { name: "Preferences" }).getByRole("button", { name: "Reset Window Layout" }),
  },
];

async function seedPopulatedStates(page: Page) {
  await page.addInitScript(() => {
    const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
    mocks.commandOverrides.overview_load = {
      scopes: [
        { scope: "global", skills: 5, mcp: 2, rules: 0, locks: 2, last_edited: "2026-11-04T10:00:00Z", disabled: 0, store_ready: true },
        { scope: "user", skills: 11, mcp: 4, rules: 1, locks: 0, last_edited: "2026-11-06T08:00:00Z", disabled: 0, store_ready: true },
        { scope: "project", skills: 5, mcp: 1, rules: 1, locks: 0, last_edited: "2026-11-07T09:00:00Z", disabled: 1, store_ready: true },
      ],
      effective: { skills: 21, mcp: 7, rules: 2, shadowed_skills: 2, disabled: 1 },
      attention: [{ item_id: "itm_visual_skill", title: "Code Review has changed", subtitle: "Review the file diff", kind: "drift_event", screen: "drift", row_id: "itm_visual_skill" }],
      attention_total: 1,
      changes: [],
      user_path: "/sandbox/user",
      global_store: { path: "/sandbox/global", source: "environment", status: "available", reason: null, needs_elevation: false, locks: [] },
      startup_warnings: [],
    };
    mocks.commandOverrides.drift_snapshot = {
      rows: [{ event_id: 17n, kind: "drifted", item_id: "itm_visual_skill", item_name: "Code Review", agent: "codex", scope: "project", path: "/sandbox/Projects/AgenTuna/.agents/skills/code-review/SKILL.md", detected_at: "2026-11-06T10:00:00Z", changed_file_count: 1, can_adopt: true, can_restore: true, link_target: "/sandbox/Projects/AgenTuna/.config-studio/skills/code-review" }],
      in_sync_count: 41,
      watched_count: 42,
      watcher: { status: "active", path: null, error: null },
    };
  });
}

/** Inspect rendered boxes rather than just document scrollWidth: the shell clips x overflow. */
async function layoutProblems(page: Page) {
  return page.evaluate(() => {
    const describe = (element: Element) => `${element.tagName.toLowerCase()}${element.id ? `#${element.id}` : ""}${element.getAttribute("class") ? `.${element.getAttribute("class")?.trim().replace(/\s+/g, ".")}` : ""}`;
    const visible = (element: Element) => {
      const style = getComputedStyle(element);
      const box = element.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && box.width > 1 && box.height > 1 && style.clipPath !== "inset(50%)";
    };
    const inHorizontalScroller = (element: Element) => {
      for (let parent = element.parentElement; parent; parent = parent.parentElement) {
        const style = getComputedStyle(parent);
        if ((style.overflowX === "auto" || style.overflowX === "scroll") && parent.scrollWidth > parent.clientWidth + 1) return true;
      }
      return false;
    };
    const horizontal = Array.from(document.querySelectorAll("body *")).filter((element) => {
      if (!visible(element) || inHorizontalScroller(element)) return false;
      const box = element.getBoundingClientRect();
      return box.left < -1 || box.right > window.innerWidth + 1;
    }).map((element) => {
      const box = element.getBoundingClientRect();
      return `${describe(element)} [${Math.round(box.left)}, ${Math.round(box.right)}]`;
    });
    const controls = Array.from(document.querySelectorAll('[data-slot="shell-topbar"] button')).filter(visible);
    const overlappingControls: string[] = [];
    for (let index = 0; index < controls.length; index += 1) {
      const a = controls[index];
      if (!a) continue;
      const first = a.getBoundingClientRect();
      for (const b of controls.slice(index + 1)) {
        const second = b.getBoundingClientRect();
        if (first.left < second.right - 1 && first.right > second.left + 1 && first.top < second.bottom - 1 && first.bottom > second.top + 1) {
          overlappingControls.push(`${a.getAttribute("aria-label") ?? a.textContent?.trim()} overlaps ${b.getAttribute("aria-label") ?? b.textContent?.trim()}`);
        }
      }
    }
    const overlappingText: string[] = [];
    for (const cell of document.querySelectorAll('[role="cell"], td, th')) {
      if (!visible(cell)) continue;
      const cellBox = cell.getBoundingClientRect();
      const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
      while (walker.nextNode()) {
        const node = walker.currentNode;
        if (!node.textContent?.trim() || !(node.parentElement && visible(node.parentElement))) continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        const outside = Array.from(range.getClientRects()).some((box) => box.left < cellBox.left - 1 || box.right > cellBox.right + 1);
        if (!outside) continue;
        let truncated = false;
        for (let ancestor = node.parentElement; ancestor && ancestor !== cell.parentElement; ancestor = ancestor.parentElement) {
          const style = getComputedStyle(ancestor);
          const box = ancestor.getBoundingClientRect();
          if (style.textOverflow === "ellipsis" && style.overflowX === "hidden" && box.left >= cellBox.left - 1 && box.right <= cellBox.right + 1) {
            truncated = true;
            break;
          }
        }
        if (!truncated) overlappingText.push(`${describe(cell)}: ${node.textContent.trim().slice(0, 100)}`);
      }
    }
    const sideScrollers = Array.from(document.querySelectorAll("body *"))
      .filter((element) => {
        if (!visible(element)) return false;
        const style = getComputedStyle(element);
        if (style.overflowX !== "auto" && style.overflowX !== "scroll") return false;
        return element.scrollWidth > element.clientWidth + 1;
      })
      .map((element) => `${describe(element)} ${element.clientWidth}<${element.scrollWidth}`);
    // An interactive box or a data cell cut by an ancestor's overflow is
    // unreachable even though it is "rendered": the shell clips sideways.
    const clippedInteractive: string[] = [];
    const interactive = document.querySelectorAll(
      '[data-slot="button"],[role="cell"],[role="columnheader"],[role="menuitem"],input,select,textarea',
    );
    for (const element of interactive) {
      if (!visible(element)) continue;
      const box = element.getBoundingClientRect();
      for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
        const style = getComputedStyle(ancestor);
        if (style.overflowX !== "hidden" && style.overflowX !== "clip") continue;
        const bounds = ancestor.getBoundingClientRect();
        if (box.left < bounds.left - 1 || box.right > bounds.right + 1) {
          clippedInteractive.push(`${describe(element)} clipped by ${describe(ancestor)}`);
          break;
        }
      }
    }
    const navButtons = Array.from(document.querySelectorAll('nav[aria-label="Workspace"] button')).filter(visible);
    const navProblems = navButtons
      .filter((button) => {
        const box = button.getBoundingClientRect();
        return box.left < -1 || box.right > window.innerWidth + 1 || box.width < 24 || box.height < 20;
      })
      .map((button) => {
        const box = button.getBoundingClientRect();
        return `${button.getAttribute("aria-label") ?? button.textContent?.trim()} [${Math.round(box.width)}x${Math.round(box.height)}]`;
      });
    const scrolling = document.scrollingElement;
    return {
      documentWidth: scrolling?.scrollWidth ?? 0,
      clientWidth: scrolling?.clientWidth ?? 0,
      horizontal,
      overlappingText,
      overlappingControls,
      sideScrollers,
      clippedInteractive,
      navProblems,
    };
  });
}

async function expectReachable(action: Locator) {
  await expect(action).toBeVisible();
  await action.scrollIntoViewIfNeeded();
  await expect(action).toBeInViewport({ ratio: 1 });
  const obstruction = await action.evaluate((element) => {
    const box = element.getBoundingClientRect();
    for (let parent = element.parentElement; parent; parent = parent.parentElement) {
      const style = getComputedStyle(parent);
      const bounds = parent.getBoundingClientRect();
      if ((style.overflowX === "hidden" || style.overflowX === "clip" || style.overflowX === "auto" || style.overflowX === "scroll") && (box.left < bounds.left - 1 || box.right > bounds.right + 1)) return `horizontally clipped by ${parent.tagName}`;
      if ((style.overflowY === "hidden" || style.overflowY === "clip" || style.overflowY === "auto" || style.overflowY === "scroll") && (box.top < bounds.top - 1 || box.bottom > bounds.bottom + 1)) return `vertically clipped by ${parent.tagName}`;
    }
    const hit = document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2);
    return hit && !element.contains(hit) ? `covered by ${hit.tagName}` : null;
  });
  expect(obstruction, "action must be fully reachable without clipping or occlusion").toBeNull();
}

for (const size of sizes) {
  test.describe(`${size.width}x${size.height}`, () => {
    test.use({ viewport: size, deviceScaleFactor: 1 });
    test.beforeEach(async ({ page }, testInfo) => {
      test.skip(testInfo.project.name !== "1x", "Compact viewports are covered once at 1x.");
      await seedPopulatedStates(page);
      await page.goto("/");
      await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
    });

    for (const screen of screens) {
      test(screen.name, async ({ page }, testInfo) => {
        await screen.open(page);
        await expect(screen.ready(page)).toBeVisible();
        await page.evaluate(() => document.fonts.ready);
        const screenshot = await page.screenshot({ animations: "disabled", caret: "hide" });
        await testInfo.attach(`${screen.name}-${size.width}x${size.height}`, { body: screenshot, contentType: "image/png" });
        if (screen.baseline) {
          await expect(page).toHaveScreenshot(`${screen.baseline}-${size.width}x${size.height}.png`, { animations: "disabled", caret: "hide" });
        }
        const problems = await layoutProblems(page);
        expect.soft(problems.documentWidth, "document must not scroll horizontally").toBeLessThanOrEqual(problems.clientWidth);
        expect.soft(problems.horizontal, "elements outside the viewport (excluding horizontal scrollers)").toEqual([]);
        expect.soft(problems.overlappingControls, "top-bar controls must not overlap").toEqual([]);
        expect.soft(problems.overlappingText, "table text crossing a cell must be ellipsized").toEqual([]);
        expect.soft(problems.clippedInteractive, "controls and cells must not be cut by a clipping ancestor").toEqual([]);
        expect.soft(problems.navProblems, "workspace navigation must stay fully visible").toEqual([]);
        if (screen.fitsWithoutSideScroll === true) {
          expect.soft(problems.sideScrollers, "a list must fit the window instead of scrolling sideways").toEqual([]);
        }
        await expectReachable(screen.action(page));
        const primaryActions = page.locator('main [data-slot="button"][data-variant="primary"]');
        for (let index = 0; index < await primaryActions.count(); index += 1) {
          const action = primaryActions.nth(index);
          if (await action.isVisible()) await expectReachable(action);
        }
        if (screen.name === "skills-list" || screen.name === "mcp-list") {
          const rowAction = page.getByRole("button", { name: screen.name === "skills-list" ? "Actions for code-review" : "Actions for Visual server" });
          await expectReachable(rowAction);
          await rowAction.click();
          const menu = page.getByRole("menu", { name: screen.name === "skills-list" ? "Actions for code-review" : "Actions for Visual server" });
          await expect(menu.getByRole("menuitem", { name: "Open" })).toBeVisible();
          // A menu must open inside the window and outside every clipping box.
          await expect(menu).toBeInViewport({ ratio: 1 });
          const menuObstruction = await menu.evaluate((element) => {
            const box = element.getBoundingClientRect();
            for (let ancestor = element.parentElement; ancestor; ancestor = ancestor.parentElement) {
              const style = getComputedStyle(ancestor);
              const bounds = ancestor.getBoundingClientRect();
              const clipsX = style.overflowX === "hidden" || style.overflowX === "clip" || style.overflowX === "auto" || style.overflowX === "scroll";
              const clipsY = style.overflowY === "hidden" || style.overflowY === "clip" || style.overflowY === "auto" || style.overflowY === "scroll";
              if (clipsX && (box.left < bounds.left - 1 || box.right > bounds.right + 1)) return `horizontally clipped by ${ancestor.tagName}`;
              if (clipsY && (box.top < bounds.top - 1 || box.bottom > bounds.bottom + 1)) return `vertically clipped by ${ancestor.tagName}`;
            }
            return null;
          });
          expect(menuObstruction, "row menu must not be clipped").toBeNull();
          // Every item, not just the first: a menu taller than its scroll
          // container used to lose the last ones behind the table's edge.
          const items = menu.getByRole("menuitem");
          const last = items.nth((await items.count()) - 1);
          await last.scrollIntoViewIfNeeded();
          await expect(last).toBeInViewport({ ratio: 1 });
          const lastHit = await last.evaluate((element) => {
            const box = element.getBoundingClientRect();
            const hit = document.elementFromPoint((box.left + box.right) / 2, (box.top + box.bottom) / 2);
            return hit !== null && element.contains(hit) ? null : (hit?.tagName ?? "nothing");
          });
          expect(lastHit, "the last menu item must be clickable, not covered").toBeNull();
          await page.keyboard.press("Escape");
        }
      });
    }
    for (const kind of ["skill", "server"] as const) {
      test(`${kind} names ellipsize without crossing the next column`, async ({ page }) => {
        const name = `long-${kind}-name-${"configuration".repeat(12)}`;
        await page.evaluate(({ kind, name }) => {
          const mocks = (window as typeof window & { __AT_TEST_MOCKS__: { commandOverrides: Record<string, unknown> } }).__AT_TEST_MOCKS__;
          if (kind === "skill") {
            mocks.commandOverrides.skills_list = {
              rows: [{ item_id: "itm_long_skill", name, scope: "user", state: "active", shadowed_by: null, description: "A long fixture", native: false, native_path: null, update_available: false, updated_at: "2026-11-06T12:00:00Z", agents: [] }],
              agents: [{ id: "codex", label: "Codex", presence: "installed" }],
              project_id: "prj_visual_fixture", project_name: "AgenTuna",
            };
          } else {
            mocks.commandOverrides.mcp_server_list = {
              servers: [{ id: "itm_long_server", slug: "long-server", name, scope: "user", transport: "stdio", native: false, locked: false, lock_scope: null, lock_reason: null, health: "ok", source_path: "/sandbox/user/mcp.json", agents: [] }],
              project_id: "prj_visual_fixture", project_name: "AgenTuna", project_agents: ["claude_code"], global_writable: true, global_reason: null,
            };
          }
        }, { kind, name });
        await navigation(page, kind === "skill" ? "Skills" : "MCP Servers").click();
        const label = page.getByTitle(name, { exact: true });
        await expect(label).toBeVisible();
        await expect(label).toHaveCSS("text-overflow", "ellipsis");
        const bounds = await label.evaluate((element) => {
          const cell = element.closest('[role="cell"]');
          return {
            contentWidth: element.scrollWidth,
            width: element.clientWidth,
            right: element.getBoundingClientRect().right,
            cellRight: cell?.getBoundingClientRect().right ?? 0,
          };
        });
        expect(bounds.contentWidth, "fixture must actually overflow its label").toBeGreaterThan(bounds.width);
        expect(bounds.right, "long text must stay within its table cell").toBeLessThanOrEqual(bounds.cellRight + 1);
        expect((await layoutProblems(page)).overlappingText).toEqual([]);
      });
    }
  });
}
test("MCP table and headers fit at desktop widths", async ({ page }) => {
  for (const width of [1440, 1280]) {
    await page.setViewportSize({ width, height: 859 });
    await page.goto("/");
    await navigation(page, "MCP Servers").click();
    const table = page.locator('[role="table"][aria-label="MCP Servers"]');
    await expect(table).toBeVisible();
    const geometry = await table.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      const headers = [...element.querySelectorAll('[role="columnheader"]')];
      return {
        right: rect.right,
        viewport: window.innerWidth,
        clippedHeader: headers.some((header) => {
          const headerRect = header.getBoundingClientRect();
          return headerRect.right > window.innerWidth || header.scrollWidth > header.clientWidth + 1;
        }),
      };
    });
    expect(geometry.right).toBeLessThanOrEqual(geometry.viewport);
    expect(geometry.clippedHeader).toBe(false);
  }
});
