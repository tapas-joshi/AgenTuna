import { describe, expect, test, vi } from "vitest";
import { applyTheme, DEFAULT_THEME_MODE, getThemeTokens, type FontSizeToken } from "../theme";
import {
  applyFontScale,
  createLocalStorageFontScaleStore,
  DEFAULT_FONT_SCALE,
  FONT_SCALES,
  FONT_SCALE_STORAGE_KEY,
  installFontScale,
  isFontScale,
  scalePixels,
} from "./scale";

function memoryStorage(initial?: string) {
  const values = new Map<string, string>();
  if (initial !== undefined) {
    values.set(FONT_SCALE_STORAGE_KEY, initial);
  }
  return {
    values,
    getItem: (key: string): string | null => values.get(key) ?? null,
    setItem: (key: string, value: string): void => {
      values.set(key, value);
    },
  };
}

function createRoot(): HTMLElement {
  return document.createElement("div");
}

function textVariables(root: HTMLElement): Map<string, string> {
  const out = new Map<string, string>();
  for (const size of Object.keys(getThemeTokens(DEFAULT_THEME_MODE).fontSize)) {
    out.set(size, root.style.getPropertyValue(`--at-text-${size}`));
  }
  return out;
}

describe("font scale tokens", () => {
  test("offers exactly the four documented scales", () => {
    expect(FONT_SCALES).toEqual([90, 100, 110, 125]);
    expect(DEFAULT_FONT_SCALE).toBe(100);
    for (const scale of FONT_SCALES) {
      expect(isFontScale(scale)).toBe(true);
    }
    expect(isFontScale(87)).toBe(false);
    expect(isFontScale("110")).toBe(false);
    expect(isFontScale(Number.NaN)).toBe(false);
  });

  test("scales to whole pixels and never below one", () => {
    expect(scalePixels(11, 90)).toBe(10);
    expect(scalePixels(11, 100)).toBe(11);
    expect(scalePixels(11, 110)).toBe(12);
    expect(scalePixels(11, 125)).toBe(14);
    expect(scalePixels(26, 125)).toBe(33);
    expect(scalePixels(2, 90)).toBe(2);
    expect(scalePixels(1, 90)).toBe(1);
  });

  test("rewrites every length token in the active theme", () => {
    const root = createRoot();
    applyFontScale(root, "light", 125);

    const tokens = getThemeTokens("light");
    expect(root.getAttribute("data-font-scale")).toBe("125");
    expect(textVariables(root).size).toBe(Object.keys(tokens.fontSize).length);
    for (const [size, value] of textVariables(root)) {
      const base = Number.parseFloat(tokens.fontSize[Number(size) as FontSizeToken]);
      expect(value).toMatch(/^\d+px$/);
      expect(Number.parseInt(value, 10)).toBeGreaterThan(base);
    }
    for (const step of Object.keys(tokens.space)) {
      expect(root.style.getPropertyValue(`--at-space-${step}`)).toMatch(/^\d+px$/);
    }
    for (const radius of Object.keys(tokens.radius)) {
      expect(root.style.getPropertyValue(`--at-radius-${radius}`)).toMatch(/^\d+px$/);
    }
  });

  test("90% shrinks type but keeps small spacing usable", () => {
    const root = createRoot();
    applyFontScale(root, "dark", 90);

    expect(root.style.getPropertyValue("--at-text-11")).toBe("10px");
    expect(root.style.getPropertyValue("--at-text-26")).toBe("23px");
    expect(root.style.getPropertyValue("--at-space-2")).toBe("2px");
    expect(root.style.getPropertyValue("--at-radius-3")).toBe("3px");
  });

  test("does not touch colour tokens", () => {
    const root = createRoot();
    applyFontScale(root, "dark", 125);
    expect(root.style.getPropertyValue("--at-color-surface")).toBe("");
  });

  test("composes with applyTheme in either order after a re-apply", () => {
    const root = createRoot();
    applyTheme(root, "dark");
    applyFontScale(root, "dark", 125);
    const scaled = textVariables(root);

    applyTheme(root, "light");
    applyFontScale(root, "light", 125);
    expect(textVariables(root)).toEqual(scaled);
  });
});

describe("persisted font scale", () => {
  test("defaults to 100 when nothing is stored", () => {
    const root = createRoot();
    const storage = memoryStorage();
    const controller = installFontScale(root, "dark", createLocalStorageFontScaleStore(storage));

    expect(controller.get()).toBe(100);
    expect(root.getAttribute("data-font-scale")).toBe("100");
    expect(root.style.getPropertyValue("--at-text-16")).toBe("16px");
  });

  test("ignores an invalid stored value", () => {
    const storage = memoryStorage("87");
    const controller = installFontScale(
      createRoot(),
      "dark",
      createLocalStorageFontScaleStore(storage),
    );
    expect(controller.get()).toBe(100);
  });

  test("persists a set scale and restores it on restart", () => {
    const storage = memoryStorage();
    const firstRoot = createRoot();
    const first = installFontScale(firstRoot, "dark", createLocalStorageFontScaleStore(storage));
    first.set(125);

    expect(storage.values.get(FONT_SCALE_STORAGE_KEY)).toBe("125");
    expect(firstRoot.style.getPropertyValue("--at-text-16")).toBe("20px");

    // A new process: same storage, fresh root.
    const secondRoot = createRoot();
    const second = installFontScale(secondRoot, "dark", createLocalStorageFontScaleStore(storage));
    expect(second.get()).toBe(125);
    expect(secondRoot.getAttribute("data-font-scale")).toBe("125");
    expect(secondRoot.style.getPropertyValue("--at-text-16")).toBe("20px");
  });

  test("re-applies the active scale after a theme switch", () => {
    const storage = memoryStorage();
    const root = createRoot();
    const controller = installFontScale(root, "dark", createLocalStorageFontScaleStore(storage));
    controller.set(110);

    applyTheme(root, "light");
    controller.apply("light");
    expect(root.getAttribute("data-font-scale")).toBe("110");
    expect(root.style.getPropertyValue("--at-text-16")).toBe("18px");
  });

  test("ignores an unsupported scale", () => {
    const write = vi.fn();
    const controller = installFontScale(createRoot(), "dark", { read: () => undefined, write });
    controller.set(87 as 90);
    expect(controller.get()).toBe(100);
    expect(write).not.toHaveBeenCalled();
  });

  test("storage failures fall back to memory instead of throwing", () => {
    const storage = {
      getItem: (): string => {
        throw new Error("blocked");
      },
      setItem: (): void => {
        throw new Error("blocked");
      },
    };
    const root = createRoot();
    const controller = installFontScale(root, "dark", createLocalStorageFontScaleStore(storage));
    controller.set(125);
    expect(controller.get()).toBe(125);
    expect(root.getAttribute("data-font-scale")).toBe("125");
  });
});
