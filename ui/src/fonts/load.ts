/**
 * Font availability check with fallbacks (TU-024).
 *
 * The bundled woff2 files are declared in `fonts.css`; this module verifies at
 * startup that the webview actually resolved them. When a file is missing the
 * theme stacks already fall back to Helvetica/SF and `ui-monospace`, so the app
 * keeps launching and rendering every screen; the only visible difference is a
 * single warning naming the families that are unavailable.
 *
 * `document.fonts` is injected so tests can simulate missing files without a
 * rendering engine.
 */

/** One family the app expects to load from the bundle. */
export interface FontFamilySpec {
  /** CSS family name as declared in `fonts.css` and the theme stack. */
  readonly family: string;
  /** Text used to probe the face; it must cover glyphs the family owns. */
  readonly sample: string;
  /** Fallback named in the warning, for example `Helvetica/SF`. */
  readonly fallback: string;
}

/** Families loaded from the bundle, in probe order. */
export const REQUIRED_FONT_FAMILIES: readonly FontFamilySpec[] = [
  { family: "Inter", sample: "AgenTuna 0123456789", fallback: "Helvetica/SF" },
  { family: "JetBrains Mono", sample: "0O1Il|{}", fallback: "ui-monospace" },
];

/** The parts of `FontFaceSet` the check needs. */
export interface FontFaceSetLike {
  /** Resolves when the initial font load finishes, successful or not. */
  readonly ready?: Promise<unknown>;
  /** Forces a face to load; rejects or resolves with no faces when missing. */
  load?(font: string, text?: string): Promise<unknown>;
  /** Reports whether a face for the descriptor is available. */
  check?(font: string, text?: string): boolean;
}

/** Outcome of {@link ensureFonts}. */
export interface FontLoadResult {
  /** False when the webview has no Font Loading API and nothing could be verified. */
  readonly checked: boolean;
  /** Families that resolved from the bundle. */
  readonly loaded: readonly string[];
  /** Families that fell back to the system stack. */
  readonly missing: readonly string[];
}

/** Options for {@link ensureFonts}. */
export interface FontLoadOptions {
  /** Font face set to probe; defaults to `document.fonts`. */
  readonly fontFaceSet?: FontFaceSetLike | null;
  /** Warning sink; defaults to `console.warn`. */
  readonly warn?: (message: string) => void;
}

function documentFonts(): FontFaceSetLike | null {
  const fonts = (globalThis.document as Document & { fonts?: FontFaceSetLike }).fonts;
  return fonts ?? null;
}

/**
 * Checks every bundled family and warns once when any is unavailable.
 *
 * Never rejects: a missing font must not stop the app from starting.
 *
 * @param options - Injected font face set and warning sink.
 */
export async function ensureFonts(options: FontLoadOptions = {}): Promise<FontLoadResult> {
  const fonts = options.fontFaceSet === undefined ? documentFonts() : options.fontFaceSet;
  if (fonts === null || fonts?.check === undefined) {
    return { checked: false, loaded: [], missing: [] };
  }

  await fonts.ready?.catch(() => undefined);

  const loaded: string[] = [];
  const missing: FontFamilySpec[] = [];
  for (const spec of REQUIRED_FONT_FAMILIES) {
    const descriptor = `400 16px "${spec.family}"`;
    // `load` resolves with the faces it found and rejects only on a bad
    // descriptor; availability is decided by `check`.
    await fonts.load?.(descriptor, spec.sample).catch(() => undefined);
    if (fonts.check(descriptor, spec.sample)) {
      loaded.push(spec.family);
    } else {
      missing.push(spec);
    }
  }

  if (missing.length > 0) {
    const names = missing.map((spec) => spec.family).join(", ");
    const fallbacks = missing.map((spec) => spec.fallback).join(" and ");
    const warn = options.warn ?? ((message: string) => console.warn(message));
    warn(`AgenTuna fonts unavailable (${names}); using ${fallbacks}.`);
  }

  return { checked: true, loaded, missing: missing.map((spec) => spec.family) };
}
