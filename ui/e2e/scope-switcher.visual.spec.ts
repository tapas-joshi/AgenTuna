import type { Page } from "@playwright/test";
import { expect, test } from "./support/tauri-mock";

type ScopeVisualState = "normal" | "active" | "disabled" | "locked" | "local-hidden";
type ThemeMode = "dark" | "light";

interface ScopeVisualFixture {
  readonly projectId: string | null;
  readonly scope: "global" | "user" | "project" | "local";
  readonly options: {
    readonly global_writable: boolean;
    readonly global_reason: string | null;
    readonly local_available: boolean;
    readonly local_reason: string | null;
    readonly has_project: boolean;
  };
}

async function openState(page: Page, fixture: ScopeVisualFixture, theme: ThemeMode): Promise<void> {
  await page.addInitScript(({ projectId, scope, options }) => {
    const mocks = (window as typeof window & {
      __AT_TEST_MOCKS__: {
        settings: { values: Record<string, unknown> };
        commandOverrides: Record<string, unknown>;
      };
    }).__AT_TEST_MOCKS__;
    mocks.settings.values.last_project_id = projectId;
    if (projectId !== null) mocks.settings.values[`last_scope.${projectId}`] = scope;
    mocks.commandOverrides.instruction_create_options = options;
  }, { ...fixture, theme });
  await page.goto("/");
  await page.evaluate((themeMode) => {
    const root = document.documentElement;
    for (const property of Array.from(root.style)) {
      if (property.startsWith("--at-color-")) root.style.removeProperty(property);
    }
    root.setAttribute("data-theme", themeMode);
    root.style.colorScheme = themeMode;
  }, theme);
  await expect(page.getByRole("navigation", { name: "Workspace" })).toBeVisible();
}

const fixtures: Readonly<Record<ScopeVisualState, ScopeVisualFixture>> = {
  normal: {
    projectId: "prj_visual_fixture",
    scope: "user",
    options: { global_writable: true, global_reason: null, local_available: true, local_reason: null, has_project: true },
  },
  active: {
    projectId: "prj_visual_fixture",
    scope: "local",
    options: { global_writable: true, global_reason: null, local_available: true, local_reason: null, has_project: true },
  },
  disabled: {
    projectId: null,
    scope: "user",
    options: { global_writable: true, global_reason: null, local_available: false, local_reason: "Choose a project first", has_project: false },
  },
  locked: {
    projectId: "prj_visual_fixture",
    scope: "user",
    options: { global_writable: false, global_reason: "Global store is not available", local_available: true, local_reason: null, has_project: true },
  },
  "local-hidden": {
    projectId: "prj_visual_fixture",
    scope: "user",
    options: { global_writable: true, global_reason: null, local_available: false, local_reason: "Local scope needs a Git work tree", has_project: true },
  },
};

for (const state of Object.keys(fixtures) as ScopeVisualState[]) {
  for (const theme of ["dark", "light"] as const) {
    test(`scope switcher ${state} state in ${theme} theme`, async ({ page }) => {
      await openState(page, fixtures[state], theme);
      const group = page.getByRole("radiogroup", { name: "Scope" }).first();
      const project = group.getByRole("radio", { name: "Project" });
      const global = group.getByRole("radio", { name: "Global" });
      const local = group.getByRole("radio", { name: "Local (only you, this repo)" });

      if (state === "disabled") {
        await expect(project).toHaveAttribute("aria-disabled", "true");
        await expect(project).toHaveAttribute("aria-describedby", "scope-project-required");
      } else if (state === "locked") {
        await expect(global).toHaveAttribute("aria-disabled", "true");
      } else if (state === "active") {
        await expect(local).toHaveAttribute("aria-checked", "true");
      } else if (state === "local-hidden") {
        await expect(local).toHaveCount(0);
      } else {
        await expect(group.getByRole("radio", { name: "User" })).toHaveAttribute("aria-checked", "true");
      }

      await expect(group).toHaveScreenshot(`scope-switcher-${state}-${theme}.png`, { animations: "disabled", caret: "hide" });
    });
  }
}
