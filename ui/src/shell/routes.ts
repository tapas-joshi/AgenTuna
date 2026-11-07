/**
 * Shell navigation model (TU-006).
 *
 * The seven F01 sections are the permanent sidebar entries. Each section owns
 * one or more TU-010 route ids; detail and flow routes (skill detail, MCP
 * form, add-skill, migrate) highlight the section they belong to so the
 * sidebar always shows where the user is.
 *
 * This module is pure data and mapping: it owns no state, performs no IO and
 * knows nothing about `AppState` (TU-009) or the router (TU-010).
 */

import type { StateRouteId } from "../components/states";
/** Stable URL-safe ids accepted by the application router. */
export const ROUTE_IDS = [
  "overview",
  "skills",
  "skill-detail",
  "mcp-list",
  "mcp-detail",
  "mcp-form",
  "effective",
  "drift",
  "instructions",
  "agents",
  "add-skill",
  "migrate",
  "settings",
  "secrets",
] as const;

/** A stable route id used by deep links and route history. */
export type RouteId = (typeof ROUTE_IDS)[number];

/** Parses a stable route id without accepting arbitrary webview strings. */
export function parseRouteId(value: string): RouteId | null {
  return (ROUTE_IDS as readonly string[]).includes(value) ? value as RouteId : null;
}

/** Returns the display id for a route; ids round-trip through {@link parseRouteId}. */
export function displayRouteId(route: RouteId): string {
  return route;
}

/** Parses supported AgenTuna item deep links. */
export function parseItemDeepLink(value: string): string | null {
  const match = /^agentuna:\/\/item\/([^/?#]+)$/.exec(value);
  return match?.[1] ?? null;
}

/** Routes which require an active project context. */
export function routeRequiresProject(route: RouteId): boolean {
  return route === "skill-detail" || route === "mcp-detail" || route === "mcp-form" ||
    route === "effective" || route === "instructions" ||
    route === "add-skill" || route === "migrate";
}

/** The seven F01 sidebar sections, in display order. */
export const SHELL_SECTIONS = [
  "overview",
  "skills",
  "mcp",
  "instructions",
  "agents",
  "effective",
  "drift",
] as const;

/** One sidebar section id. */
export type ShellSectionId = (typeof SHELL_SECTIONS)[number];

/** Visible label for a sidebar section. */
export const SHELL_SECTION_LABEL: Readonly<Record<ShellSectionId, string>> = {
  overview: "Overview",
  skills: "Skills",
  mcp: "MCP Servers",
  instructions: "Instructions",
  agents: "Agents",
  effective: "Effective config",
  drift: "Sync & drift",
};

/**
 * The route a section navigates to when its item is activated. Sections that
 * own several routes land on their list/landing route.
 */
export const SHELL_SECTION_ROUTE: Readonly<Record<ShellSectionId, StateRouteId>> = {
  overview: "overview",
  skills: "skills",
  mcp: "mcp-list",
  instructions: "instructions",
  agents: "agents",
  effective: "effective",
  drift: "drift",
};

/**
 * Routes that belong to a section but are reached through a different control
 * (a row click, a wizard step). They light the parent section without being
 * sidebar destinations themselves.
 */
const SECTION_ROUTES: Readonly<Record<ShellSectionId, readonly StateRouteId[]>> = {
  overview: ["overview"],
  skills: ["skills", "skill-detail", "add-skill"],
  mcp: ["mcp-list", "mcp-detail", "mcp-form"],
  instructions: ["instructions"],
  agents: ["agents"],
  effective: ["effective"],
  drift: ["drift"],
  // `migrate` is launched from the account/settings area, not a nav section.
};

/**
 * Returns the section a route belongs to, or `null` for routes with no
 * sidebar entry (`migrate`, `settings`).
 */
export function sectionForRoute(route: StateRouteId): ShellSectionId | null {
  for (const section of SHELL_SECTIONS) {
    if (SECTION_ROUTES[section].includes(route)) {
      return section;
    }
  }
  return null;
}

/**
 * Agents reported by detection for the "Agents on this Mac" block. The shell
 * renders whatever list it is given; detection itself is TA-031's job.
 */
export interface ShellAgentStatus {
  /** Stable agent id (`claude-code`, `codex`, `omp`, `cursor`, …). */
  readonly id: string;
  /** Display name. */
  readonly label: string;
  /** Whether the agent's CLI/config was found on this machine. */
  readonly detected: boolean;
}

/** Account identity shown in the sidebar footer card. */
export interface ShellAccount {
  /** Display name or handle; the avatar initial derives from it. */
  readonly name: string;
  /** Secondary line under the name, for example `~/ · macOS`. */
  readonly detail?: string;
}
