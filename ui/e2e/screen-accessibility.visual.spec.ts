import { createRequire } from "node:module";
import { dirname } from "node:path";
import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

const require = createRequire(import.meta.url);
const jestAxePath = require.resolve("jest-axe");
const axeScriptPath = require.resolve("axe-core/axe.min.js", { paths: [dirname(jestAxePath)] });

type AxeViolation = {
  readonly id: string;
  readonly impact: string | null;
  readonly help: string;
  readonly nodes: readonly { readonly target: readonly string[]; readonly failureSummary: string | null }[];
};

async function audit(page: Page, screenName: string): Promise<string[]> {
  const violations = await page.evaluate(async () => {
    const axe = (window as typeof window & {
      axe?: { run: () => Promise<{ violations: AxeViolation[] }> };
    }).axe;
    if (!axe) throw new Error("axe-core did not load into the app webview");
    return (await axe.run()).violations;
  });
  return violations.map((violation) =>
    `${screenName}: ${violation.id} (${violation.impact ?? "unknown"}) ${violation.help}\n${violation.nodes.map((node) => `${node.target.join(" ")}: ${node.failureSummary ?? ""}`).join("\n")}`,
  );
}

async function openMigration(page: Page) {
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await page.keyboard.press("Meta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("Import native items");
  await palette.getByRole("option", { name: /Import native items/ }).click();
  await expect(page.getByRole("heading", { name: /items outside AgenTuna/ })).toBeVisible();
}

test("axe audits every built screen, migration step, picker and modal", async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto("/");
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
  await expect(page.getByRole("heading", { name: /^Overview/ })).toBeVisible();
  await page.addScriptTag({ path: axeScriptPath });
  const failures: string[] = [];
  const check = async (name: string) => failures.push(...await audit(page, name));

  await check("Overview");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await check("Skills");
  await page.getByRole("tab", { name: "Presets" }).click();
  await expect(page.getByRole("heading", { name: "Presets", exact: true })).toBeVisible();
  await check("Presets");
  await page.getByRole("tab", { name: "Skills" }).click();
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("button", { name: "Actions for code-review" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();
  await expect(page.getByRole("heading", { name: "Code Review", exact: true }).first()).toBeVisible();
  await check("Skill detail");
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Move to scope…" }).click();
  await check("Skill action dialog");
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await expect(page.getByRole("main", { name: "MCP Servers" })).toBeVisible();
  await check("MCP list");
  await page.getByRole("button", { name: "Actions for Visual server" }).click();
  await page.getByRole("menuitem", { name: "Open" }).click();
  await expect(page.getByRole("heading", { name: "Visual server" })).toBeVisible();
  await check("MCP detail");
  await page.getByRole("button", { name: "Edit" }).click();
  await expect(page.getByRole("main", { name: /^Edit / })).toBeVisible();
  await check("MCP form");
  await page.getByRole("main", { name: /^Edit / }).getByRole("button", { name: "Cancel" }).first().click();

  await page.getByRole("button", { name: "Instructions", exact: true }).click();
  await expect(page.getByRole("main", { name: "Instructions" })).toBeVisible();
  await check("Instructions");
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  await check("Agents");
  await page.getByRole("button", { name: "Effective config", exact: true }).click();
  await check("Effective config");
  await page.getByRole("button", { name: "Sync & drift", exact: true }).click();
  await check("Drift");

  await openMigration(page);
  await check("Migrate scan");
  const migration = page.getByRole("main", { name: "Move into AgenTuna" });
  await migration.getByRole("checkbox", { name: "Select all items" }).check();
  await migration.getByRole("button", { name: "Continue with 3" }).click();
  await expect(page.getByRole("heading", { name: "Resolve conflicts" })).toBeVisible();
  await check("Migrate resolve");
  await page.getByLabel("Keep both").check();
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("heading", { name: "Plan the move" })).toBeVisible();
  await check("Migrate plan");
  await page.getByRole("button", { name: "Apply 3 items" }).click();
  await check("Migrate confirmation dialog");
  const confirm = page.getByRole("dialog", { name: "Confirm migration" });
  await confirm.getByRole("checkbox").check();
  await confirm.getByRole("button", { name: "Apply migration" }).click();
  await expect(page.getByRole("heading", { name: /3 items now live in AgenTuna/ })).toBeVisible();
  await check("Migrate done");

  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Preferences" })).toBeVisible();
  await check("Preferences");
  await page.getByRole("button", { name: "Skills", exact: true }).click();
  await page.getByRole("button", { name: "Add", exact: true }).click();
  await page.getByRole("menuitem", { name: "New skill" }).click();
  await expect(page.getByRole("heading", { name: "Add a skill" })).toBeVisible();
  await check("Add-skill wizard");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Meta+p");
  await expect(page.getByRole("dialog", { name: "Switch project" })).toBeVisible();
  await check("Project picker");
  await page.keyboard.press("Escape");

  expect(failures, failures.join("\n\n")).toEqual([]);
});
