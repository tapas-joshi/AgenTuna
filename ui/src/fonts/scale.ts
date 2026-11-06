/**
 * User font scale (TU-024).
 *
 * The design uses fixed pixel tokens, so the OS text-size setting cannot reach
 * them. The app therefore offers an explicit scale (90/100/110/125%) that
 * rewrites every length token — font sizes, spacing and radii — so labels grow
 * without clipping and padding keeps its proportions. Colours and font stacks
 * are untouched.
 *
 * Scaling rounds to whole CSS pixels: fractional font sizes blur at 1.5x and
 * 2x, which the story forbids.
 */

import { getThemeTokens, type ThemeMode } from "../theme";

/** The scales offered in the preferences screen. */
export type FontScale = 90 | 100 | 110 | 125;

/** Every allowed scale, smallest first. */
export const FONT_SCALES: readonly FontScale[] = [90, 100, 110, 125];

/** Scale used when nothing is stored. */
export const DEFAULT_FONT_SCALE: FontScale = 100;

/** Web storage key holding the persisted scale. */
export const FONT_SCALE_STORAGE_KEY = "agentuna.font_scale";

/** Narrows `value` to a supported scale. */
export function isFontScale(value: unknown): value is FontScale {
  return typeof value === "number" && FONT_SCALES.includes(value as FontScale);
}

/**
 * Scales one pixel value and rounds it to whole CSS pixels.
 *
 * @param pixels - Base value from the token table, in pixels.
 * @param scale - Active font scale.
 * @returns At least 1 pixel, so hairlines and radii never vanish.
 */
export function scalePixels(pixels: number, scale: FontScale): number {
  return Math.max(1, Math.round((pixels * scale) / 100));
}

/** Storage seam for the persisted scale. */
export interface FontScaleStore {
  /** Returns the stored scale, or `undefined` when absent or invalid. */
  read(): FontScale | undefined;
  /** Persists `scale`; a storage failure must not break the UI. */
  write(scale: FontScale): void;
}

/**
 * Web-storage backed scale store.
 *
 * @param storage - Storage to use; defaults to `localStorage`. Absent or
 * throwing storage (blocked cookies, private mode) degrades to memory-only.
 */
export function createLocalStorageFontScaleStore(
  storage?: Pick<Storage, "getItem" | "setItem">,
): FontScaleStore {
  const target = storage ?? safeLocalStorage();
  return {
    read: () => {
      if (target === undefined) {
        return undefined;
      }
      try {
        const raw = target.getItem(FONT_SCALE_STORAGE_KEY);
        const parsed = raw === null ? Number.NaN : Number.parseInt(raw, 10);
        return isFontScale(parsed) ? parsed : undefined;
      } catch {
        return undefined;
      }
    },
    write: (scale) => {
      if (target === undefined) {
        return;
      }
      try {
        target.setItem(FONT_SCALE_STORAGE_KEY, String(scale));
      } catch {
        // A full or blocked storage only costs persistence, not the current session.
      }
    },
  };
}

function safeLocalStorage(): Pick<Storage, "getItem" | "setItem"> | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/**
 * Writes the scaled length tokens to `root`.
 *
 * Call after `applyTheme`: `applyTheme` writes the base token values, this
 * function replaces every length token with its scaled value.
 *
 * @param root - Element that owns the tokens, normally `document.documentElement`.
 * @param mode - Theme whose base values are scaled; light and dark share lengths.
 * @param scale - Scale to apply.
 */
export function applyFontScale(root: HTMLElement, mode: ThemeMode, scale: FontScale): void {
  const tokens = getThemeTokens(mode);
  root.setAttribute("data-font-scale", String(scale));

  for (const [size, value] of Object.entries(tokens.fontSize)) {
    root.style.setProperty(`--at-text-${size}`, `${scalePixels(Number.parseFloat(value), scale)}px`);
  }
  for (const [step, value] of Object.entries(tokens.space)) {
    root.style.setProperty(`--at-space-${step}`, `${scalePixels(Number.parseFloat(value), scale)}px`);
  }
  for (const [step, value] of Object.entries(tokens.radius)) {
    root.style.setProperty(`--at-radius-${step}`, `${scalePixels(Number.parseFloat(value), scale)}px`);
  }
}

/** Live scale state for the app. */
export interface FontScaleController {
  /** Active scale. */
  get(): FontScale;
  /** Applies and persists `scale`. */
  set(scale: FontScale): void;
  /** Re-applies the active scale for `mode`; call after a theme switch. */
  apply(mode: ThemeMode): void;
}

/**
 * Applies the stored scale at startup and returns the controller.
 *
 * @param root - Element that owns the tokens.
 * @param mode - Theme applied at startup.
 * @param store - Persistence seam; defaults to web storage.
 */
export function installFontScale(
  root: HTMLElement,
  mode: ThemeMode,
  store: FontScaleStore = createLocalStorageFontScaleStore(),
): FontScaleController {
  let current = store.read() ?? DEFAULT_FONT_SCALE;
  let currentMode = mode;

  const controller: FontScaleController = {
    get: () => current,
    set: (scale) => {
      if (!isFontScale(scale)) {
        return;
      }
      current = scale;
      store.write(scale);
      applyFontScale(root, currentMode, current);
    },
    apply: (nextMode) => {
      currentMode = nextMode;
      applyFontScale(root, currentMode, current);
    },
  };

  controller.apply(mode);
  return controller;
}
