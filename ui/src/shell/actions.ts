import type { StateRouteId } from "../components/states";
/** Stable actions shared by the native menu, keyboard shortcuts and command palette. */
export const ACTION_IDS = {
  overview: "navigate:overview",
  skills: "navigate:skills",
  servers: "navigate:mcp-list",
  effective: "navigate:effective",
  drift: "navigate:drift",
  instructions: "navigate:instructions",
  agents: "navigate:agents",
  projectPicker: "project-picker",
  preferences: "preferences",
  addSkill: "add-skill",
  addServer: "add-mcp-server",
  addRule: "add-rule-file",
  addMenu: "add-menu",
  shortcuts: "keyboard-shortcuts",
  docs: "documentation",
  doctor: "run-doctor",
  import: "import",
  rescan: "projects-rescan",
  installCli: "install-cli",
  palette: "command-palette",
  save: "save",
} as const;

export type AppActionId = (typeof ACTION_IDS)[keyof typeof ACTION_IDS];
 

/** Shared navigation targets used by native menus, shortcuts and palette actions. */
export const ACTION_ROUTES: Readonly<Record<string, StateRouteId>> = {
  [ACTION_IDS.overview]: "overview",
  [ACTION_IDS.skills]: "skills",
  [ACTION_IDS.servers]: "mcp-list",
  [ACTION_IDS.effective]: "effective",
  [ACTION_IDS.drift]: "drift",
  [ACTION_IDS.instructions]: "instructions",
  [ACTION_IDS.agents]: "agents",
  [ACTION_IDS.preferences]: "settings",
  [ACTION_IDS.addSkill]: "add-skill",
  [ACTION_IDS.addServer]: "mcp-form",
  [ACTION_IDS.addRule]: "instructions",
  [ACTION_IDS.import]: "migrate",
};

/** Resolve an action id to its shared route target, if it navigates. */
export function routeForAction(id: string): StateRouteId | null {
  return ACTION_ROUTES[id] ?? null;
}

/** Action map consumed to render the keyboard help sheet and dispatch key events. */
export const SHORTCUTS = [
  { key: "k", modifier: "primary", action: ACTION_IDS.palette, label: "Command palette" },
  { key: "p", modifier: "primary", action: ACTION_IDS.projectPicker, label: "Choose project" },
  { key: "s", modifier: "primary", action: ACTION_IDS.save, label: "Save active editor" },
  { key: "n", modifier: "primary", action: ACTION_IDS.addMenu, label: "Open Add menu" },
  { key: ",", modifier: "primary", action: ACTION_IDS.preferences, label: "Preferences" },
  ...(["overview", "skills", "mcp-list", "effective", "drift", "instructions", "agents"] as const).map((route, index) => ({
    key: String(index + 1), modifier: "primary" as const, action: `navigate:${route}`, label: `Open ${route}`,
  })),
  ...(["global", "user", "project", "local"] as const).map((scope, index) => ({
    key: String(index + 1), modifier: "primary-shift" as const, action: `scope:${scope}`, label: `Select ${scope} scope`,
  })),
  { key: "Escape", modifier: "none", action: "overlay:close", label: "Close top overlay" },
] as const;

/** Returns the user-facing key hint for an action. */
export function shortcutHint(action: string, platform: "mac" | "other" = "mac"): string | undefined {
  const shortcut = SHORTCUTS.find((item) => item.action === action);
  if (!shortcut) return undefined;
  const primary = platform === "mac" ? "⌘" : "Ctrl+";
  if (shortcut.modifier === "none") return shortcut.key;
  return `${primary}${shortcut.modifier === "primary-shift" ? "⇧" : ""}${shortcut.key.toUpperCase()}`;
}
