/**
 * Fixture matrix for the state pattern (TU-021).
 *
 * Every route from TU-010, plus the shell and overlay surfaces, is described in
 * the four states a screen can show: empty, loading, error and populated. The
 * fixture test fails when a route or state is missing, so no screen can quietly
 * ship without copy for one of them. `offline` is carried as a fifth entry for
 * surfaces that can run without the network.
 *
 * Screens and their visual tests read from this table instead of repeating
 * placeholder strings, and the 1x/2x snapshot pass renders the same entries.
 */

import { errorDetailsText, toErrorReport } from "./error-model";
import type { StateDetails, StateVariant } from "./types";

/** The four states every screen must define. */
export const SCREEN_STATES = ["empty", "loading", "error", "populated"] as const;

/** One of the four states every screen must define. */
export type ScreenState = (typeof SCREEN_STATES)[number];

/**
 * Stable route ids from TU-010, in display order. The router owns the parser;
 * this list only proves the state matrix covers each id.
 */
export const STATE_ROUTE_IDS = [
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
] as const;

/** A TU-010 route id. */
export type StateRouteId = (typeof STATE_ROUTE_IDS)[number];

/** Surfaces outside the route list that still render these states. */
export const STATE_CHROME_IDS = ["shell", "overlay"] as const;

/** Apps chrome that renders the states outside a route. */
export type StateChromeId = (typeof STATE_CHROME_IDS)[number];

/** Every surface the matrix covers. */
export const STATE_SURFACE_IDS = [...STATE_ROUTE_IDS, ...STATE_CHROME_IDS] as const;

/** A route or a chrome surface with state copy. */
export type StateSurfaceId = (typeof STATE_SURFACE_IDS)[number];

/** One state view rendered without its callbacks. */
export interface StateFixture {
  /** Which variant to render. */
  readonly variant: StateVariant;
  /** Heading copy. */
  readonly title: string;
  /** Supporting sentence. */
  readonly body: string;
  /** Primary action label, when the state offers one. */
  readonly action?: string;
  /** Expandable technical payload; only the error fixture carries one. */
  readonly details?: StateDetails;
}

/** The populated case: how much fixture data the real screen renders. */
export interface PopulatedFixture {
  /** Number of row-like objects the screen shows. */
  readonly rows: number;
  /** What the populated screen contains, for reviewers and snapshot names. */
  readonly note: string;
}

/** All states for one surface. */
export interface SurfaceStateFixture {
  /** No data yet, with one action. */
  readonly empty: StateFixture;
  /** First load, with skeleton rows or a spinner. */
  readonly loading: StateFixture;
  /** A failed load with human copy and technical details. */
  readonly error: StateFixture;
  /** No network; local data still renders. */
  readonly offline: StateFixture;
  /** Real fixture rows rendered off the screen widget. */
  readonly populated: PopulatedFixture;
}

const SAMPLE_ERROR = toErrorReport({
  code: "E_PERMISSION_DENIED",
  message: "Permission denied (os error 13)",
  user_message: "Cannot write /Users/ana/.claude/skills/tdd/SKILL.md because the folder is read-only.",
  path: "/Users/ana/.claude/skills/tdd/SKILL.md",
  operation_id: "op_01J8Z6R2K3M4N5P6Q7R8S9T0V1",
  source_chain: ["failed to open /Users/ana/.claude/skills/tdd/SKILL.md", "Permission denied"],
  remedy: ["reveal_in_finder", "copy_details"],
  retryable: false,
});

const SAMPLE_DETAILS: StateDetails = {
  summary: "Technical details (E_PERMISSION_DENIED)",
  text: errorDetailsText(SAMPLE_ERROR),
};

interface SurfaceMeta {
  readonly emptyTitle: string;
  readonly emptyBody: string;
  readonly action: string;
  readonly loadingTitle: string;
  readonly errorTitle: string;
  readonly rows: number;
  readonly populatedNote: string;
}

const OFFLINE: Omit<StateFixture, "variant"> = {
  title: "Working offline",
  body: "AgenTuna can't reach the network. Local configuration still works; remote features wait until you reconnect.",
  action: "Open Settings",
};

const SURFACE_META: Readonly<Record<StateSurfaceId, SurfaceMeta>> = {
  overview: {
    emptyTitle: "No projects yet",
    emptyBody: "Add a project folder to see which skills, servers and rules each agent loads.",
    action: "Add project",
    loadingTitle: "Loading overview…",
    errorTitle: "Couldn't load the overview",
    rows: 6,
    populatedNote: "Scope cards and the recent-changes list",
  },
  skills: {
    emptyTitle: "No skills in this scope",
    emptyBody: "Import a skill from git, or create one from scratch.",
    action: "Add skill",
    loadingTitle: "Loading skills…",
    errorTitle: "Couldn't load skills",
    rows: 26,
    populatedNote: "Skill rows with per-agent switches",
  },
  "skill-detail": {
    emptyTitle: "This skill has no content",
    emptyBody: "The SKILL.md file is empty. Add instructions, then save.",
    action: "Edit skill",
    loadingTitle: "Loading skill…",
    errorTitle: "Couldn't load this skill",
    rows: 1,
    populatedNote: "Header, deployment matrix and rendered SKILL.md",
  },
  "mcp-list": {
    emptyTitle: "No MCP servers yet",
    emptyBody: "Add a server to share its tools between agents.",
    action: "Add server",
    loadingTitle: "Loading servers…",
    errorTitle: "Couldn't load MCP servers",
    rows: 8,
    populatedNote: "Server rows with health and reach",
  },
  "mcp-detail": {
    emptyTitle: "This server has no tools",
    emptyBody: "The health check returned no tools. Run it again, or check the server's command.",
    action: "Run health check",
    loadingTitle: "Loading server…",
    errorTitle: "Couldn't load this server",
    rows: 1,
    populatedNote: "Header, health panel and tool list",
  },
  "mcp-form": {
    emptyTitle: "No servers to edit",
    emptyBody: "Create a server first, then come back to change its settings.",
    action: "Add server",
    loadingTitle: "Loading server settings…",
    errorTitle: "Couldn't load the server form",
    rows: 1,
    populatedNote: "Connection fields, secrets and scope picker",
  },
  effective: {
    emptyTitle: "Nothing resolves in this scope",
    emptyBody: "No skill, server or rule is enabled here yet.",
    action: "Review skills",
    loadingTitle: "Resolving effective config…",
    errorTitle: "Couldn't resolve the effective config",
    rows: 14,
    populatedNote: "Winning scope per item with the why-trace",
  },
  drift: {
    emptyTitle: "Nothing drifted",
    emptyBody: "Every agent file matches its AgenTuna copy.",
    action: "Check again",
    loadingTitle: "Checking for drift…",
    errorTitle: "Couldn't check for drift",
    rows: 3,
    populatedNote: "Drifted files with adopt and restore",
  },
  instructions: {
    emptyTitle: "No instructions file",
    emptyBody: "AgenTuna can generate AGENTS.md and CLAUDE.md from one source.",
    action: "Create instructions",
    loadingTitle: "Loading instructions…",
    errorTitle: "Couldn't load instructions",
    rows: 4,
    populatedNote: "Managed blocks and the generated file preview",
  },
  agents: {
    emptyTitle: "No agents detected",
    emptyBody: "Install Claude Code, Codex or omp, then detect agents again.",
    action: "Detect agents",
    loadingTitle: "Detecting agents…",
    errorTitle: "Agent detection failed",
    rows: 3,
    populatedNote: "Detected agents with paths and capabilities",
  },
  "add-skill": {
    emptyTitle: "No source selected",
    emptyBody: "Pick git, a local file or the library to import a skill.",
    action: "Choose source",
    loadingTitle: "Opening the add-skill flow…",
    errorTitle: "Couldn't start the add-skill flow",
    rows: 2,
    populatedNote: "Source options and the details form",
  },
  migrate: {
    emptyTitle: "Nothing to migrate",
    emptyBody: "No native skills or servers were found outside AgenTuna.",
    action: "Scan again",
    loadingTitle: "Scanning native items…",
    errorTitle: "Couldn't scan for native items",
    rows: 12,
    populatedNote: "Scanned items with conflict resolution",
  },
  settings: {
    emptyTitle: "No settings loaded",
    emptyBody: "Settings arrive from the backend. Reload them to edit health, backups and network policy.",
    action: "Reload settings",
    loadingTitle: "Loading settings…",
    errorTitle: "Couldn't load settings",
    rows: 9,
    populatedNote: "Health, backups, network and appearance sections",
  },
  shell: {
    emptyTitle: "No workspace open",
    emptyBody: "Choose a project folder to start managing configuration.",
    action: "Open project",
    loadingTitle: "Opening workspace…",
    errorTitle: "Couldn't open the workspace",
    rows: 3,
    populatedNote: "Sidebar, scope switcher and status bar",
  },
  overlay: {
    emptyTitle: "No matches",
    emptyBody: "Try a different search, or pick a command from the list.",
    action: "Clear search",
    loadingTitle: "Searching…",
    errorTitle: "Couldn't run that command",
    rows: 5,
    populatedNote: "Command palette results",
  },
};

function fixturesFor(meta: SurfaceMeta): SurfaceStateFixture {
  return {
    empty: { variant: "empty", title: meta.emptyTitle, body: meta.emptyBody, action: meta.action },
    loading: { variant: "loading", title: meta.loadingTitle, body: "" },
    error: { variant: "error", title: meta.errorTitle, body: "", details: SAMPLE_DETAILS },
    offline: { variant: "offline", title: OFFLINE.title, body: OFFLINE.body, action: OFFLINE.action },
    populated: { rows: meta.rows, note: meta.populatedNote },
  };
}

/** The complete fixture matrix, keyed by route or chrome surface id. */
export const SURFACE_STATE_FIXTURES: Readonly<Record<StateSurfaceId, SurfaceStateFixture>> =
  Object.fromEntries(
    STATE_SURFACE_IDS.map((id) => [id, fixturesFor(SURFACE_META[id])]),
  ) as Record<StateSurfaceId, SurfaceStateFixture>;

/**
 * Returns one line per missing or malformed entry in a state matrix.
 *
 * Every screen test can call this with its own matrix; the fixture test calls it
 * with {@link SURFACE_STATE_FIXTURES} and fails on any row, so a new route that
 * forgets a state is caught rather than rendered blank.
 */
export function validateSurfaceFixtures(
  matrix: Readonly<Record<StateSurfaceId, SurfaceStateFixture>>,
): string[] {
  const problems: string[] = [];
  for (const id of STATE_SURFACE_IDS) {
    const fixture = matrix[id];
    if (fixture === undefined) {
      problems.push(`${id}: missing surface`);
      continue;
    }
    for (const state of ["empty", "loading", "error"] as const) {
      const entry = fixture[state];
      if (entry === undefined) {
        problems.push(`${id}: missing ${state}`);
      } else if (entry.variant !== state) {
        problems.push(`${id}: ${state} entry uses variant ${entry.variant}`);
      }
    }
    if (fixture.populated === undefined) {
      problems.push(`${id}: missing populated`);
    } else if (fixture.populated.rows <= 0) {
      problems.push(`${id}: populated has no rows`);
    }
    if (fixture.offline === undefined) {
      problems.push(`${id}: missing offline`);
    } else if (fixture.offline.variant !== "offline") {
      problems.push(`${id}: offline entry uses variant ${fixture.offline.variant}`);
    }
  }
  return problems;
}

/** The sample error every `error` fixture shows, reusable in tests and storybooks. */
export { SAMPLE_ERROR };
