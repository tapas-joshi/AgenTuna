/**
 * App shell (TU-006).
 *
 * `Shell` draws the permanent chrome — sidebar, top bar, routed body and the
 * overlay host — around whatever screen the router (TU-010) renders. It owns
 * no business logic: navigation, project picking, scope changes and the
 * palette all leave through callbacks, and scope state itself belongs to
 * TA-053/TU-040.
 *
 * Layout contract:
 * - `<nav>` (sidebar) and `<header>` (top bar) are landmarks with names.
 * - `<main id="shell-main">` is the routed region; a skip link targets it.
 * - The body region is `min-w-0 flex-1` so a wide table never pushes the top
 *   bar; the whole frame is `h-full` so nothing scrolls the chrome away.
 * - At 900px the sidebar keeps its 200px rail, the picker truncates its label
 *   and the palette hint keeps its width — no control overlaps.
 */

import type { JSX, ReactNode } from "react";
import { cn } from "../components/cn";
import type { ScopeToken, ThemeMode } from "../theme";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import type { ShellAccount, ShellAgentStatus, ShellSectionId } from "./routes";
import { sectionForRoute } from "./routes";
import type { StateRouteId } from "../components/states";

/** Props accepted by {@link Shell}. */
export interface ShellProps {
  /** Active TU-010 route id; drives the highlighted sidebar section. */
  readonly route: StateRouteId;
  /** Visible title for the top bar and document title. */
  readonly title: string;
  /** Routed screen body; the shell does not inspect it. */
  readonly children: ReactNode;
  /** Called with the section's landing route id when a nav item activates. */
  readonly onNavigate: (route: StateRouteId) => void;
  /** Called when the project picker trigger is activated. */
  readonly onProjectPicker: () => void;
  /** Active project label for the picker trigger; `null` = no project. */
  readonly projectLabel?: string | null;
  /** The TU-040 scope switcher widget, rendered in the top bar. */
  readonly scopeSwitcher?: ReactNode;
  /** Current scope, used for the fallback scope dot when no widget is given. */
  readonly scope?: ScopeToken;
  /** Called when the palette hint is activated. */
  readonly onPalette: () => void;
  /** Agents detected on this machine for the sidebar block. */
  readonly agents?: readonly ShellAgentStatus[];
  /** Account identity for the sidebar footer card. */
  readonly account?: ShellAccount;
  /** App version line under the logo. */
  readonly versionLabel?: string;
  /** Theme mode for scope dots. */
  readonly themeMode?: ThemeMode;
  /** Palette shortcut modifier for the hint label. */
  readonly paletteModifier?: "meta" | "ctrl";
  /** Overlay layers (palette, project picker, dialogs) rendered above the frame. */
  readonly overlays?: ReactNode;
  readonly className?: string;
}

/**
 * Renders the application chrome around a routed screen.
 *
 * @example
 * ```tsx
 * <Shell
 *   route="skills"
 *   title="Skills"
 *   onNavigate={router.push}
 *   onProjectPicker={openProjectPicker}
 *   onPalette={openPalette}
 *   projectLabel="ai-coding-config-studio"
 *   scopeSwitcher={<ScopeSwitcher … />}
 * >
 *   <SkillsScreen />
 * </Shell>
 * ```
 */
export function Shell({
  route,
  title,
  children,
  onNavigate,
  onProjectPicker,
  projectLabel = null,
  scopeSwitcher,
  scope,
  onPalette,
  agents = [],
  account,
  versionLabel,
  themeMode = "dark",
  paletteModifier = "meta",
  overlays,
  className,
}: ShellProps): JSX.Element {
  const activeSection: ShellSectionId | null = sectionForRoute(route);

  return (
    <div
      data-slot="shell"
      className={cn("flex h-full min-h-0 w-full", className)}
    >
      {/* Skip link: first tabbable element, jumps past the chrome. */}
      <a
        href="#shell-main"
        data-slot="shell-skip-link"
        className={cn(
          "sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4",
          "focus:z-50 focus:rounded-6 focus:border focus:border-border",
          "focus:bg-raised focus:px-8 focus:py-4 focus:text-12 focus:text-text",
        )}
      >
        Skip to content
      </a>

      <Sidebar
        activeSection={activeSection}
        onNavigate={onNavigate}
        agents={agents}
        account={account}
        versionLabel={versionLabel}
        themeMode={themeMode}
      />

      <div className="flex min-w-0 flex-1 flex-col">
        <TopBar
          title={title}
          projectLabel={projectLabel}
          onProjectPicker={onProjectPicker}
          scopeSwitcher={scopeSwitcher}
          scope={scope}
          themeMode={themeMode}
          onPalette={onPalette}
          paletteModifier={paletteModifier}
        />
        {/*
         * `min-h-0` + `overflow-y-auto` keeps the body scrollable inside the
         * frame while the chrome stays put; `min-w-0` lets wide tables scroll
         * horizontally instead of overflowing the window.
         */}
        <main
          id="shell-main"
          data-slot="shell-main"
          className="min-h-0 min-w-0 flex-1 overflow-y-auto overflow-x-hidden"
        >
          {children}
        </main>
      </div>

      {overlays}
    </div>
  );
}
