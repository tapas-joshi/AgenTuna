/**
 * Public surface of the theme module.
 *
 * Import `tokens.css` once (see `main.tsx`) and use these exports to read the
 * token table or switch modes at runtime:
 *
 * ```ts
 * import { applyTheme, ThemeTokens } from "./theme";
 * applyTheme(document.documentElement, "light");
 * ```
 */

export {
  applyTheme,
  DEFAULT_THEME_MODE,
  getThemeTokens,
  ThemeTokens,
  tokenVariables,
} from "./tokens";
export type {
  FontSizeToken,
  RadiusToken,
  ScopeToken,
  SpacingToken,
  ThemeColors,
  ThemeMode,
  TokenVariable,
} from "./tokens";
