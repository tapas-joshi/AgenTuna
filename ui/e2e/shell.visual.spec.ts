import { expect, test } from "@playwright/test";

// Freeze browser time for reproducible empty-state shell rendering.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {
        invoke: async (command: string) => {
          if (command === "app_info") return { name: "AgenTuna", version: "0.1.0" };
          if (command === "settings_load") return { schema_version: 1, values: {} };
          if (command === "projects_list") return [];
          if (command === "plugin:event|listen") return 1;
          return null;
        },
        transformCallback: () => 1,
        unregisterCallback: () => undefined,
      },
    });
    const fixedNow = 1_798_560_000_000;
    const NativeDate = Date;
    class FrozenDate extends NativeDate {
      constructor(...args: ConstructorParameters<typeof Date>) {
        if (args.length === 0) super(fixedNow);
        else super(...args);
      }
      static now() { return fixedNow; }
    }
    Object.defineProperty(window, "Date", { value: FrozenDate });
  });
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
});

test("production shell empty states across representative screens", async ({ page }) => {
  const routes = ["Overview", "Skills", "MCP Servers", "Instructions", "Agents", "Effective config", "Sync & drift"];
  for (const route of routes) {
    await page.getByRole("button", { name: route, exact: true }).click();
    await expect(page.locator("#shell-main h1")).toBeVisible();
    await expect(page.locator("body")).toHaveScreenshot(`shell-${route.toLowerCase().replaceAll(" ", "-")}.png`, {
      animations: "disabled",
      caret: "hide",
      maxDiffPixelRatio: 0.001,
    });
  }
});
