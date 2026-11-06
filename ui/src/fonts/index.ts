/**
 * Public surface of the typography module (TU-024).
 *
 * ```ts
 * import "./fonts/fonts.css";
 * import { applyTheme } from "./theme";
 * import { bootstrapFonts } from "./fonts";
 *
 * applyTheme(document.documentElement, "dark");
 * const fonts = bootstrapFonts(document.documentElement, "dark");
 * fonts.scale.set(110); // persists through restart
 * ```
 *
 * The `data-density` attribute (`1x`, `1.5x`, `2x`), the `--at-pixel-ratio`
 * variable and the `data-fonts` attribute (`bundled`, `fallback`, `unknown`)
 * are the observable state other modules and screenshot tests can read.
 */

export { bootstrapFonts } from "./bootstrap";
export type { FontBootstrap, FontBootstrapOptions } from "./bootstrap";
export { applyDensity, pixelRatioBucket, watchDensity } from "./density";
export type {
  DensityBucket,
  DensityController,
  DensityInfo,
  DensityWindow,
  MediaQueryListLike,
} from "./density";
export { ensureFonts, REQUIRED_FONT_FAMILIES } from "./load";
export type {
  FontFaceSetLike,
  FontFamilySpec,
  FontLoadOptions,
  FontLoadResult,
} from "./load";
export {
  applyFontScale,
  createLocalStorageFontScaleStore,
  DEFAULT_FONT_SCALE,
  FONT_SCALES,
  FONT_SCALE_STORAGE_KEY,
  installFontScale,
  isFontScale,
  scalePixels,
} from "./scale";
export type { FontScale, FontScaleController, FontScaleStore } from "./scale";
