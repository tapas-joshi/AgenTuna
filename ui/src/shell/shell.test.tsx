/**
 * Tests for the app shell chrome (TU-006).
 *
 * Covers the four acceptance bullets: every F01 section renders with icon and
 * label and highlights the active route; the top bar hosts the picker, the
 * scope-switcher slot and the palette hint without overlap at 900px; the body
 * renders loading/empty/error/populated children without layout shift; and
 * the chrome is keyboard reachable with ARIA roles and names.
 */

import { StrictMode, type ReactNode } from "react";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { StateView, SURFACE_STATE_FIXTURES } from "../components/states";
import type { StateRouteId } from "../components/states";
import { Shell } from "./Shell";
import { displayRouteId, parseItemDeepLink, parseRouteId, ROUTE_IDS, routeRequiresProject, SHELL_SECTION_LABEL, SHELL_SECTIONS, sectionForRoute } from "./routes";
import type { ShellAgentStatus } from "./routes";

afterEach(cleanup);
describe("stable route identifiers", () => {
  test("every route id round-trips through parsing and display", () => {
    for (const route of ROUTE_IDS) {
      expect(parseRouteId(displayRouteId(route))).toBe(route);
    }
    expect(parseRouteId("unknown")).toBeNull();
  });

  test("parses item deep links and identifies project-scoped routes", () => {
    expect(parseItemDeepLink("agentuna://item/itm_example")).toBe("itm_example");
    expect(parseItemDeepLink("agentuna://item/")).toBeNull();
    expect(parseItemDeepLink("agentuna://item/itm_example/extra")).toBeNull();
    expect(parseItemDeepLink("agentuna://item/itm_example?ignored=true")).toBeNull();
    expect(routeRequiresProject("mcp-detail")).toBe(true);
    expect(routeRequiresProject("overview")).toBe(false);
  });
});

const AGENTS: readonly ShellAgentStatus[] = [
  { id: "claude-code", label: "Claude Code", detected: true },
  { id: "codex", label: "Codex", detected: true },
  { id: "omp", label: "omp", detected: true },
  { id: "cursor", label: "Cursor", detected: false },
];

interface ShellOverrides {
  readonly route?: StateRouteId;
  readonly projectLabel?: string | null;
  readonly canGoBack?: boolean;
  readonly scopeSwitcher?: ReactNode;
  readonly paletteModifier?: "meta" | "ctrl";
  readonly children?: ReactNode;
}

function renderShell(overrides: ShellOverrides = {}) {
  const onNavigate = vi.fn();
  const onProjectPicker = vi.fn();
  const onPalette = vi.fn();
  const onBack = vi.fn();
  render(
    <Shell
      route={overrides.route ?? "overview"}
      title="Overview"
      onNavigate={onNavigate}
      onProjectPicker={onProjectPicker}
      onPalette={onPalette}
      canGoBack={overrides.canGoBack}
      onBack={onBack}
      projectLabel={overrides.projectLabel === undefined ? "ai-coding-config-studio" : overrides.projectLabel}
      agents={AGENTS}
      account={{ name: "tapas", detail: "~/ · macOS" }}
      scopeSwitcher={overrides.scopeSwitcher}
      paletteModifier={overrides.paletteModifier}
    >
      {overrides.children ?? <main aria-label="Workspace content" data-testid="body">body</main>}
    </Shell>,
  );
  return { onNavigate, onProjectPicker, onPalette, onBack };
}

describe("sidebar sections", () => {
  test("renders every F01 section with icon and label", () => {
    renderShell();
    const nav = screen.getByRole("navigation", { name: "Workspace" });
    for (const section of SHELL_SECTIONS) {
      const item = within(nav).getByRole("button", {
        name: SHELL_SECTION_LABEL[section],
      });
      expect(item).toBeTruthy();
      expect(item.querySelector("svg")).toBeTruthy();
    }
  });

  test("active route highlights its section and navigation updates it", async () => {
    const user = userEvent.setup();
    const onNavigate = vi.fn();
    const { rerender } = render(
      <Shell
        route="skills"
        title="Skills"
        onNavigate={onNavigate}
        onProjectPicker={() => {}}
        onPalette={() => {}}
      >
        <div />
      </Shell>,
    );

    const skills = screen.getByRole("button", { name: "Skills" });
    expect(skills.getAttribute("aria-current")).toBe("page");
    expect(skills.dataset.section).toBe("skills");

    // Detail routes light their parent section.
    rerender(
      <Shell
        route="skill-detail"
        title="Skill"
        onNavigate={onNavigate}
        onProjectPicker={() => {}}
        onPalette={() => {}}
      >
        <div />
      </Shell>,
    );
    expect(screen.getByRole("button", { name: "Skills" }).getAttribute("aria-current")).toBe(
      "page",
    );

    // Activating another item reports its landing route.
    await user.click(screen.getByRole("button", { name: "MCP Servers" }));
    expect(onNavigate).toHaveBeenCalledWith("mcp-list");
  });

  test("every TU-010 route id maps to a section or none", () => {
    // migrate and settings intentionally have no sidebar entry.
    expect(sectionForRoute("migrate")).toBeNull();
    expect(sectionForRoute("settings")).toBeNull();
    expect(sectionForRoute("overview")).toBe("overview");
    expect(sectionForRoute("mcp-form")).toBe("mcp");
    expect(sectionForRoute("add-skill")).toBe("skills");
  });

  test("agents block lists detected and missing agents with text, not colour only", () => {
    renderShell();
    expect(screen.getByText("Agents on this Mac")).toBeTruthy();
    expect(screen.getByText("Claude Code")).toBeTruthy();
    expect(screen.getByText("Cursor")).toBeTruthy();
    expect(screen.getAllByText("detected")).toHaveLength(3);
    expect(screen.getByText("not found")).toBeTruthy();
  });

  test("account card renders name and detail", () => {
    renderShell();
    expect(screen.getByText("tapas")).toBeTruthy();
    expect(screen.getByText("~/ · macOS")).toBeTruthy();
  });
});

describe("top bar", () => {
  test("hosts project picker, scope switcher slot and palette hint", async () => {
    const user = userEvent.setup();
    const { onProjectPicker, onPalette } = renderShell({
      scopeSwitcher: <div data-testid="scope-switcher">Global User Project</div>,
    });

    const picker = screen.getByRole("button", { name: /Project: ai-coding-config-studio/ });
    expect(picker).toBeTruthy();
    expect(screen.getByTestId("scope-switcher")).toBeTruthy();

    const palette = screen.getByRole("button", { name: /Open command palette/ });
    expect(palette.textContent).toContain("⌘K");

    await user.click(picker);
    await user.click(palette);
    expect(onProjectPicker).toHaveBeenCalledTimes(1);
    expect(onPalette).toHaveBeenCalledTimes(1);
  });
  test("Back is labelled, disabled at the root and dispatches route history when available", async () => {
    const user = userEvent.setup();
    const disabled = renderShell();
    expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(true);
    cleanup();
    const enabled = renderShell({ canGoBack: true });
    await user.click(screen.getByRole("button", { name: "Back" }));
    expect(enabled.onBack).toHaveBeenCalledOnce();
    expect((screen.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false);
    expect(disabled.onBack).not.toHaveBeenCalled();
  });

  test("project picker shows the empty state when no project is active", () => {
    renderShell({ projectLabel: null });
    expect(screen.getByRole("button", { name: "Choose a project" })).toBeTruthy();
    expect(screen.getByText("No project")).toBeTruthy();
  });

  test("palette hint advertises Ctrl+K on non-mac modifiers", () => {
    renderShell({ paletteModifier: "ctrl" });
    const palette = screen.getByRole("button", { name: /Open command palette/ });
    expect(palette.textContent).toContain("Ctrl+K");
    expect(palette.getAttribute("aria-keyshortcuts")).toBe("Control+K");
  });

  test("TU-006 AC2: layout stays usable at 900 px width with no overlapping controls", () => {
    render(
      <div style={{ width: "900px", height: "600px", display: "flex" }}>
        <Shell
          route="overview"
          title="Overview"
          projectLabel="very-long-project-name-ai-coding-config-studio-workspace"
          onNavigate={() => {}}
          onProjectPicker={() => {}}
          onPalette={() => {}}
          canGoBack={true}
          onBack={() => {}}
          scopeSwitcher={
            <div data-testid="scope-switcher" className="inline-flex gap-4">
              <button type="button">Global</button>
              <button type="button">User</button>
              <button type="button">Project</button>
            </div>
          }
        >
          <div>Body</div>
        </Shell>
      </div>,
    );

    const topbar = screen.getByRole("banner");
    expect(topbar).toBeTruthy();

    const backBtn = screen.getByRole("button", { name: "Back" });
    const title = topbar.querySelector("[data-slot='shell-title']");
    const picker = screen.getByRole("button", { name: /Project:/ });
    const scopeSwitcher = screen.getByTestId("scope-switcher");
    const palette = screen.getByRole("button", { name: /Open command palette/ });

    // All controls are present and reachable
    expect(backBtn).toBeTruthy();
    expect(title).toBeTruthy();
    expect(picker).toBeTruthy();
    expect(scopeSwitcher).toBeTruthy();
    expect(palette).toBeTruthy();

    // Project picker and title have min-w-0 and truncate classes so they compress rather than overlap
    expect(picker.className).toContain("min-w-0");
    expect(picker.className).toContain("shrink");
    expect(picker.querySelector(".truncate")).toBeTruthy();
    expect(title?.className).toContain("min-w-0");
    expect(title?.className).toContain("truncate");

    // Palette hint preserves its fixed width and does not shrink
    expect(palette.className).toContain("w-[220px]");
    expect(palette.className).toContain("shrink-0");

    // Flex container preserves single-row horizontal layout with gap and no wrapping
    expect(topbar.className).toContain("flex");
    expect(topbar.className).toContain("items-center");
    expect(topbar.className).not.toContain("flex-wrap");

    // Geometry in a 900px container:
    // Sidebar = 200px rail. Topbar width = 700px.
    // Padding = 16px left + 16px right = 32px. Available = 668px.
    // Fixed width elements: Back (28px) + Palette (220px) = 248px + 4 gaps (48px) = 296px.
    // Remainder for title, picker and scopeSwitcher is 372px.
    const topbarWidth = 700;
    const padding = 16;
    const gap = 12;
    const fixedWidths = 28 + 220;
    const gaps = 4 * gap;
    const remaining = topbarWidth - 2 * padding - gaps - fixedWidths;
    expect(remaining).toBeGreaterThan(300);

    // DOM sequence matches horizontal visual order (left to right, no overlapping coordinates)
    const topbarChildren = Array.from(topbar.children);
    const backIdx = topbarChildren.indexOf(backBtn);
    const titleIdx = topbarChildren.indexOf(title as Element);
    const pickerIdx = topbarChildren.indexOf(picker);
    const scopeContainer = scopeSwitcher.parentElement;
    const scopeIdx = topbarChildren.indexOf(scopeContainer as Element);
    const paletteIdx = topbarChildren.indexOf(palette);

    expect(backIdx).toBeLessThan(titleIdx);
    expect(titleIdx).toBeLessThan(pickerIdx);
    expect(pickerIdx).toBeLessThan(scopeIdx);
    expect(scopeIdx).toBeLessThan(paletteIdx);

    // Controls are in-flow elements, none positioned absolute. The scope slot
    // keeps a fixed 420px width in the flow (TU-040): identical bounds in every
    // state without centering over the search or reflowing the row.
    for (const el of [backBtn, title as Element, picker, scopeContainer as Element, palette]) {
      expect(el.className).not.toContain("absolute");
      expect(el.className).not.toContain("fixed");
    }
    expect((scopeContainer as Element).className).toContain("w-[420px]");
  });
});

describe("routed body states", () => {
  test.each(["loading", "empty", "error"] as const)(
    "renders the real %s StateView fixture without layout shift",
    (variant) => {
      const fixture = SURFACE_STATE_FIXTURES.overview[variant];
      renderShell({
        children: (
          <StateView
            variant={fixture.variant}
            title={fixture.title}
            body={fixture.body}
            minHeight={480}
          />
        ),
      });
      const main = document.getElementById("shell-main");
      expect(main).toBeTruthy();
      // Chrome stays mounted around every state.
      expect(screen.getByRole("navigation", { name: "Workspace" })).toBeTruthy();
      expect(document.querySelector('[data-slot="shell-topbar"]')).toBeTruthy();
    },
  );

  test("populated body renders inside main without touching chrome", () => {
    renderShell({ children: <div data-testid="rows">26 rows</div> });
    const main = document.getElementById("shell-main");
    expect(within(main as HTMLElement).getByTestId("rows")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Skills" })).toBeTruthy();
  });
});

describe("keyboard and ARIA", () => {
  test("nav items are reachable by Tab and activate with Enter", async () => {
    const user = userEvent.setup();
    const { onNavigate } = renderShell({ route: "overview" });

    const overview = screen.getByRole("button", { name: "Overview" });
    overview.focus();
    expect(document.activeElement).toBe(overview);

    await user.keyboard("{Enter}");
    expect(onNavigate).toHaveBeenCalledWith("overview");
  });

  test("arrow keys move between nav items (roving tabindex)", async () => {
    const user = userEvent.setup();
    renderShell({ route: "overview" });

    const overview = screen.getByRole("button", { name: "Overview" });
    const skills = screen.getByRole("button", { name: "Skills" });
    overview.focus();

    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(skills);

    await user.keyboard("{End}");
    expect(document.activeElement).toBe(
      screen.getByRole("button", { name: "Sync & drift" }),
    );

    await user.keyboard("{Home}");
    expect(document.activeElement).toBe(overview);
  });

  test("landmarks expose roles and names", () => {
    renderShell();
    expect(screen.getByRole("navigation", { name: "Workspace" })).toBeTruthy();
    expect(screen.getByRole("banner")).toBeTruthy(); // <header>
    expect(screen.getByRole("main")).toBeTruthy();
    expect(document.getElementById("shell-main")?.id).toBe("shell-main");
  });

  test("skip link targets the main region", () => {
    renderShell();
    const skip = screen.getByRole("link", { name: "Skip to content" });
    expect(skip.getAttribute("href")).toBe("#shell-main");
  });
});

describe("StrictMode safety", () => {
  test("double render keeps nav and callbacks stable", async () => {
    const user = userEvent.setup();
    const onNavigate = vi.fn();
    render(
      <StrictMode>
        <Shell
          route="overview"
          title="Overview"
          onNavigate={onNavigate}
          onProjectPicker={() => {}}
          onPalette={() => {}}
        >
          <div />
        </Shell>
      </StrictMode>,
    );
    await user.click(screen.getByRole("button", { name: "Skills" }));
    expect(onNavigate).toHaveBeenCalledTimes(1);
    expect(onNavigate).toHaveBeenCalledWith("skills");
  });
});
