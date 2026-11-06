/**
 * Typography bootstrap (TU-024).
 *
 * One call from `main.tsx` applies the stored font scale, mirrors the device
 * pixel ratio and verifies that the bundled fonts resolved. It never throws and
 * never blocks the first paint: the font check runs in the background and only
 * changes the `data-fonts` attribute and one warning.
 */

import type { ThemeMode } from "../theme";
import { ensureFonts, type FontFaceSetLike, type FontLoadResult } from "./load";
import {
  installFontScale,
  type FontScaleController,
  type FontScaleStore,
} from "./scale";
import { watchDensity, type DensityController, type DensityWindow } from "./density";

/** Options for {@link bootstrapFonts}; every dependency is injectable. */
export interface FontBootstrapOptions {
  /** Persistence seam for the font scale; defaults to web storage. */
  readonly store?: FontScaleStore;
  /** Window-like density source; defaults to the real window. */
  readonly window?: DensityWindow;
  /** Font face set to probe; `null` skips the check. Defaults to `document.fonts`. */
  readonly fontFaceSet?: FontFaceSetLike | null;
  /** Warning sink for missing fonts; defaults to `console.warn`. */
  readonly warn?: (message: string) => void;
}

/** Live handles returned by {@link bootstrapFonts}. */
export interface FontBootstrap {
  /** Font scale controller, also used by the preferences screen. */
  readonly scale: FontScaleController;
  /** Density controller; `stop()` detaches the monitor listener. */
  readonly density: DensityController;
  /** Resolves with the font check result; never rejects. */
  readonly fonts: Promise<FontLoadResult>;
  /** Stops listening for monitor changes. */
  stop(): void;
}

/**
 * Applies typography for `mode`.
 *
 * Call after `applyTheme` so the scaled length tokens replace the base ones.
 *
 * @param root - Element that owns the tokens, normally `document.documentElement`.
 * @param mode - Theme applied at startup.
 * @param options - Injected seams for tests.
 */
export function bootstrapFonts(
  root: HTMLElement,
  mode: ThemeMode,
  options: FontBootstrapOptions = {},
): FontBootstrap {
  const scale = installFontScale(root, mode, options.store);
  const density = watchDensity(root, options.window);

  const fonts = ensureFonts({
    fontFaceSet: options.fontFaceSet,
    warn: options.warn,
  }).then((result) => {
    // "unknown" means the webview has no Font Loading API to verify with.
    root.setAttribute(
      "data-fonts",
      result.checked ? (result.missing.length === 0 ? "bundled" : "fallback") : "unknown",
    );
    return result;
  });

  return {
    scale,
    density,
    fonts,
    stop: () => {
      density.stop();
    },
  };
}
