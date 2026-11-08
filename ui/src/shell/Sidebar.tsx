/**
 * Sidebar chrome (TU-006).
 *
 * Permanent left rail: logo block, the seven F01 nav sections, the
 * "Agents on this Mac" detection block and the account card. It is fully
 * presentational — every action leaves through a typed callback, and the
 * active section is whatever `activeSection` the caller passes.
 *
 * Keyboard model: the nav is a single `nav` landmark containing one list of
 * buttons. Tab reaches the nav as one stop via roving tabindex; ArrowUp /
 * ArrowDown / Home / End move between items, Enter or Space activates. This
 * mirrors the WAI-ARIA toolbar/listbox pattern used by the rest of the kit.
 */

import { useCallback, useRef } from "react";
import type { JSX, KeyboardEvent } from "react";
import { cn } from "../components/cn";
import type { StateRouteId } from "../components/states";
import { getThemeTokens } from "../theme";
import type { ThemeMode } from "../theme";
import { AccountMenu } from "../overlays/menus/AccountMenu";
import type { AccountMenuAction } from "../overlays/menus/AccountMenu";
import { SECTION_ICONS } from "./icons";
import {
  SHELL_SECTION_LABEL,
  SHELL_SECTION_ROUTE,
  SHELL_SECTIONS,
  type ShellAccount,
  type ShellAgentStatus,
  type ShellSectionId,
} from "./routes";

/** Props accepted by {@link Sidebar}. */
export interface SidebarProps {
  /** Section rendered as active; `null` highlights nothing (e.g. `migrate`). */
  readonly activeSection: ShellSectionId | null;
  /** Active route id; closes menus on route changes and restores focus. */
  readonly currentRoute?: StateRouteId;
  /** Called with the section's landing route id when a nav item activates. */
  readonly onNavigate: (route: StateRouteId) => void;
  /** Agents detected on this machine, in display order. */
  readonly agents?: readonly ShellAgentStatus[];
  /** Account identity for the footer card. */
  readonly account?: ShellAccount;
  /** Account menu action forwarded to the shared application dispatcher. */
  readonly onAccountAction?: (action: AccountMenuAction) => void;
  /** Unavailable account menu actions with reasons. */
  readonly accountDisabledReasons?: Partial<Record<AccountMenuAction, string>>;
  /** App version line under the logo, for example `v0.1 · local`. */
  readonly versionLabel?: string;
  /** Active theme mode; scope dots use its palette. Defaults to `dark`. */
  readonly themeMode?: ThemeMode;
  readonly className?: string;
}

const NAV_ITEM_BASE =
  "flex w-full items-center gap-8 rounded-6 px-8 py-[6px] text-left text-13 transition-colors " +
  "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus";

/**
 * Renders the permanent sidebar.
 *
 * @example
 * ```tsx
 * <Sidebar
 *   activeSection="skills"
 *   onNavigate={(route) => router.push(route)}
 *   agents={detectedAgents}
 *   account={{ name: "tapas", detail: "~/ · macOS" }}
 * />
 * ```
 */
export function Sidebar({
  activeSection,
  onNavigate,
  agents = [],
  account,
  versionLabel = "v0.1 · local",
  currentRoute,
  onAccountAction,
  accountDisabledReasons,
  themeMode = "dark",
  className,
}: SidebarProps): JSX.Element {
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);

  const focusItem = useCallback((index: number) => {
    const clamped = Math.max(0, Math.min(index, SHELL_SECTIONS.length - 1));
    itemRefs.current[clamped]?.focus();
  }, []);

  const onNavKeyDown = useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      const current = SHELL_SECTIONS.findIndex(
        (_, i) => itemRefs.current[i] === document.activeElement,
      );
      if (current === -1) {
        return;
      }
      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          focusItem(current + 1);
          break;
        case "ArrowUp":
          event.preventDefault();
          focusItem(current - 1);
          break;
        case "Home":
          event.preventDefault();
          focusItem(0);
          break;
        case "End":
          event.preventDefault();
          focusItem(SHELL_SECTIONS.length - 1);
          break;
        default:
          break;
      }
    },
    [focusItem],
  );

  return (
    <aside
      aria-label="Sidebar"
      data-slot="shell-sidebar"
      className={cn(
        "flex h-full w-[200px] shrink-0 flex-col border-r border-border bg-sidebar",
        className,
      )}
    >
      {/* Logo block */}
      <div className="flex items-center gap-8 px-12 pt-12 pb-8">
        <span
          aria-hidden="true"
          className="flex size-24 shrink-0 items-center justify-center rounded-6 bg-accent-fill text-11 font-bold text-on-accent"
        >
          AT
        </span>
        <span className="flex min-w-0 flex-col">
          <span className="truncate text-15 font-semibold text-text">AgenTuna</span>
          <span className="truncate text-11 text-text-muted">{versionLabel}</span>
        </span>
      </div>

      {/* Primary nav */}
      <nav aria-label="Workspace" className="flex min-h-0 flex-1 flex-col px-8 pt-8">
        <span className="px-8 pb-4 text-11 font-medium tracking-wide text-text-muted uppercase">
          Workspace
        </span>
        <ul
          role="list"
          className="flex flex-col gap-2"
          onKeyDown={onNavKeyDown}
        >
          {SHELL_SECTIONS.map((section, index) => {
            const Icon = SECTION_ICONS[section];
            const active = section === activeSection;
            return (
              <li key={section}>
                <button
                  ref={(el) => {
                    itemRefs.current[index] = el;
                  }}
                  type="button"
                  data-slot="shell-nav-item"
                  data-section={section}
                  aria-current={active ? "page" : undefined}
                  tabIndex={0}
                  onMouseEnter={() => {
                    if (section === "skills") void import("../screens/skills/SkillsScreen").catch(() => undefined);
                    if (section === "mcp") void import("../screens/mcp_list/McpListScreen").catch(() => undefined);
                  }}
                  onFocus={() => {
                    if (section === "skills") void import("../screens/skills/SkillsScreen").catch(() => undefined);
                    if (section === "mcp") void import("../screens/mcp_list/McpListScreen").catch(() => undefined);
                  }}
                  onClick={() => onNavigate(SHELL_SECTION_ROUTE[section])}
                  className={cn(
                    NAV_ITEM_BASE,
                    active
                      ? "bg-raised font-medium text-text shadow-[inset_2px_0_0_var(--at-color-accent)]"
                      : "text-text-secondary hover:bg-overlay hover:text-text",
                  )}
                >
                  <span aria-hidden="true" className={cn("shrink-0", active ? "text-accent" : "text-text-muted")}>
                    <Icon />
                  </span>
                  <span className="truncate">{SHELL_SECTION_LABEL[section]}</span>
                </button>
              </li>
            );
          })}
        </ul>

        {/* Agents on this Mac */}
        {agents.length === 0 ? null : (
          <div className="mt-16 flex min-h-0 flex-col">
            <span className="px-8 pb-4 text-11 font-medium tracking-wide text-text-muted uppercase">
              Agents on this Mac
            </span>
            <ul role="list" className="flex flex-col gap-2 px-8">
              {agents.map((agent) => (
                <li
                  key={agent.id}
                  className="flex items-center justify-between gap-8 text-12"
                >
                  <span className="truncate text-text-secondary">{agent.label}</span>
                  <span className="flex shrink-0 items-center gap-4">
                    <span
                      aria-hidden="true"
                      className={cn(
                        "inline-block size-8 rounded-full",
                        agent.detected ? "bg-success" : "border",
                      )}
                      style={
                        agent.detected
                          ? undefined
                          : { borderColor: getThemeTokens(themeMode).color.scope.local }
                      }
                    />
                    <span className="text-text-muted">
                      {agent.detected ? "detected" : "not found"}
                    </span>
                  </span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </nav>

      <div className="px-8 pb-12 pt-8">
        <AccountMenu
          version={versionLabel}
          account={account}
          onAction={onAccountAction}
          disabledReasons={accountDisabledReasons}
          routeKey={currentRoute}
        />
      </div>
    </aside>
  );
}

