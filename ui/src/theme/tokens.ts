/**
 * Theme tokens for the AgenTuna UI.
 *
 * This module is the single source of truth for every colour, spacing, radius,
 * font-family and font-size value used by the Tauri/React app. Values mirror
 * `design/screens/00-design-system.png`; the CSS in `tokens.css` mirrors this
 * table and `theme-css.test.ts` fails when the two drift apart.
 *
 * Dark is the default theme. Light uses the same token names and the same
 * shapes, so components never branch on the active mode.
 */

/** Names of the two supported themes. */
export type ThemeMode = "dark" | "light";

/** Theme used when the UI starts, before any user preference is applied. */
export const DEFAULT_THEME_MODE: ThemeMode = "dark";

/** Scope colours from the design-system page. */
export type ScopeToken = "global" | "user" | "project" | "local";

/** Spacing steps in pixels (density 7: compact desktop spacing). */
export type SpacingToken = 2 | 4 | 8 | 12 | 16 | 24 | 32;

/** Corner radii in pixels. */
export type RadiusToken = 3 | 6 | 10;

/**
 * Font sizes in pixels. `11`-`22` are the UI scale from TU-001; `14`, `16`
 * and `26` are the body, section-title and page-title sizes drawn on the
 * design-system page.
 */
export type FontSizeToken = 11 | 12 | 13 | 14 | 15 | 16 | 18 | 22 | 26;

/** Colour tokens shared by every theme mode. */
export interface ThemeColors {
  /** App background. */
  readonly surface: string;
  /** Cards, tables and other content sitting on `surface`. */
  readonly raised: string;
  /** Menus, popovers and hover fills. */
  readonly overlay: string;
  /** Navigation rail, recessed below `surface`. */
  readonly sidebar: string;
  /** Hairline borders and dividers. */
  readonly border: string;
  /** Text hierarchy. `primary` carries body copy, `secondary` labels, `muted` metadata. */
  readonly text: {
    readonly primary: string;
    readonly secondary: string;
    readonly muted: string;
  };
  /** Primary action colour (CTAs, active states). */
  readonly accent: string;
  /** Success status. */
  readonly success: string;
  /** Warning status. */
  readonly warning: string;
  /** Danger and destructive actions. */
  readonly danger: string;
  /** Focus ring drawn by `:focus-visible`. */
  readonly focus: string;
  /** Scope identity colours: colour always ships with a text label. */
  readonly scope: Readonly<Record<ScopeToken, string>>;
}

/**
 * The complete token table for one theme mode.
 *
 * Every mode exposes exactly the same key paths; `themeTokens` is typed with
 * this interface so a missing key is a compile error, and `tokens.test.ts`
 * additionally checks the key sets at runtime.
 */
export interface ThemeTokens {
  /** Colour tokens. */
  readonly color: ThemeColors;
  /** Spacing scale in pixels. */
  readonly space: Readonly<Record<SpacingToken, string>>;
  /** Corner radii in pixels. */
  readonly radius: Readonly<Record<RadiusToken, string>>;
  /** Font-size scale in pixels. */
  readonly fontSize: Readonly<Record<FontSizeToken, string>>;
  /** Font stacks. */
  readonly font: {
    readonly ui: string;
    readonly mono: string;
  };
}

const INK = "#F8FAFC";
const SLATE_300 = "#CBD5E1";
const SLATE_400 = "#94A3B8";

const dark: ThemeTokens = {
  color: {
    surface: "#0F172A",
    raised: "#1B2336",
    overlay: "#272F42",
    sidebar: "#0B1222",
    border: "#475569",
    text: {
      primary: INK,
      secondary: SLATE_300,
      muted: SLATE_400,
    },
    accent: "#22C55E",
    success: "#22C55E",
    warning: "#F59E0B",
    // The design-system red (#EF4444) only reaches 4.16:1 on `raised` and
    // 3.55:1 on `overlay`; WCAG 2.1 AA for body text needs 4.5:1, so the
    // token uses the nearest passing red (red-400).
    danger: "#F87171",
    focus: "#FFFFFF",
    scope: {
      global: "#A78BFA",
      user: "#38BDF8",
      project: "#FBBF24",
      local: SLATE_400,
    },
  },
  space: {
    2: "2px",
    4: "4px",
    8: "8px",
    12: "12px",
    16: "16px",
    24: "24px",
    32: "32px",
  },
  radius: {
    3: "3px",
    6: "6px",
    10: "10px",
  },
  fontSize: {
    11: "11px",
    12: "12px",
    13: "13px",
    14: "14px",
    15: "15px",
    16: "16px",
    18: "18px",
    22: "22px",
    26: "26px",
  },
  font: {
    ui: '"Inter", "Helvetica Neue", system-ui, -apple-system, sans-serif',
    mono: '"JetBrains Mono", "Menlo", ui-monospace, "SFMono-Regular", monospace',
  },
};

const light: ThemeTokens = {
  color: {
    surface: "#F8FAFC",
    raised: "#FFFFFF",
    overlay: "#F1F5F9",
    sidebar: "#F1F5F9",
    border: "#CBD5E1",
    text: {
      primary: "#0F172A",
      secondary: "#334155",
      muted: "#475569",
    },
    accent: "#15803D",
    success: "#15803D",
    warning: "#B45309",
    danger: "#B91C1C",
    focus: "#0F172A",
    scope: {
      global: "#6D28D9",
      user: "#0369A1",
      project: "#B45309",
      local: "#475569",
    },
  },
  space: dark.space,
  radius: dark.radius,
  fontSize: dark.fontSize,
  font: dark.font,
};

/**
 * Token tables indexed by theme mode.
 *
 * Also usable as a type: `import type { ThemeTokens } from "./theme"`.
 */
export const ThemeTokens: Readonly<Record<ThemeMode, ThemeTokens>> = {
  dark,
  light,
};

/** Returns the token table for `mode`. */
export function getThemeTokens(mode: ThemeMode): ThemeTokens {
  return ThemeTokens[mode];
}

/**
 * CSS custom-property prefix for tokens. `applyTheme` and `tokens.css` both
 * write these names, so the sync test can compare them literally.
 */
const VARIABLE_PREFIX = "--at-";

/** Top-level token groups whose CSS prefix differs from their TS key. */
const GROUP_ALIASES: Readonly<Record<string, string>> = {
  fontSize: "text",
};

/** A token value mapped to the CSS custom property that carries it. */
export interface TokenVariable {
  /** Custom property name, for example `--at-color-surface`. */
  readonly name: string;
  /** Token value for the theme that produced the list. */
  readonly value: string;
}

function variableName(path: readonly string[]): string {
  const parts: string[] = [];
  for (const [index, part] of path.entries()) {
    // `color.text.primary` is the bare text colour; the other text tokens keep
    // their suffix (`--at-color-text-secondary`, `--at-color-text-muted`).
    if (part === "primary" && path[index - 1] === "text") {
      continue;
    }
    parts.push(GROUP_ALIASES[part] ?? part);
  }
  return `${VARIABLE_PREFIX}${parts.join("-")}`;
}

function isTokenGroup(value: unknown): value is object {
  return typeof value === "object" && value !== null;
}

/**
 * Flattens a token table into CSS custom properties.
 *
 * The order is stable (depth-first, insertion order), so `applyTheme` writes
 * the same sequence every time and tests can compare lists directly.
 */
export function tokenVariables(tokens: ThemeTokens): TokenVariable[] {
  const out: TokenVariable[] = [];
  const walk = (value: object, path: string[]): void => {
    for (const [key, child] of Object.entries(value)) {
      if (isTokenGroup(child)) {
        walk(child, [...path, key]);
      } else {
        out.push({ name: variableName([...path, key]), value: child });
      }
    }
  };
  walk(tokens, []);
  return out;
}

/**
 * Applies `mode` to `root`: sets the `data-theme` attribute, mirrors every
 * token as an inline custom property and updates `color-scheme` so native
 * controls (scrollbars, inputs) follow the theme.
 *
 * Writing the variables inline keeps the active values identical to
 * `ThemeTokens` even before `tokens.css` loads, and guarantees that a mode
 * switch repaints the shell, tables and overlays without a restart.
 *
 * @param root - Element that owns the theme, normally
 *   `document.documentElement`.
 * @param mode - Theme to apply.
 */
export function applyTheme(root: HTMLElement, mode: ThemeMode): void {
  const tokens = getThemeTokens(mode);
  root.setAttribute("data-theme", mode);
  root.style.colorScheme = mode;
  for (const { name, value } of tokenVariables(tokens)) {
    root.style.setProperty(name, value);
  }
}
