import { describe, expect, test, vi } from "vitest";
import { bootstrapFonts } from "./bootstrap";
import type { FontFaceSetLike } from "./load";
import type { FontScaleStore } from "./scale";
import type { DensityWindow } from "./density";

function fontSet(available: readonly string[]): FontFaceSetLike {
  return {
    ready: Promise.resolve(),
    load: async () => [],
    check: (descriptor: string) =>
      available.some((family) => descriptor.includes(`"${family}"`)),
  };
}

function memoryScaleStore(initial?: string): FontScaleStore & { stored: () => string | undefined } {
  let value = initial;
  return {
    read: () => (value === "90" ? 90 : value === "110" ? 110 : value === "125" ? 125 : undefined),
    write: (scale) => {
      value = String(scale);
    },
    stored: () => value,
  };
}

function densityWindow(ratio: number): DensityWindow & { devicePixelRatio: number } {
  return {
    devicePixelRatio: ratio,
    matchMedia: () => ({ addEventListener: () => undefined, removeEventListener: () => undefined }),
  };
}

describe("font bootstrap", () => {
  test("applies scale, density and the bundled-font state at startup", async () => {
    const root = document.createElement("div");
    const store = memoryScaleStore();
    const warn = vi.fn();
    const bootstrap = bootstrapFonts(root, "dark", {
      store,
      window: densityWindow(2),
      fontFaceSet: fontSet(["Inter", "JetBrains Mono"]),
      warn,
    });

    expect(root.getAttribute("data-font-scale")).toBe("100");
    expect(root.getAttribute("data-density")).toBe("2x");
    expect(root.style.getPropertyValue("--at-pixel-ratio")).toBe("2");

    const result = await bootstrap.fonts;
    expect(result.missing).toEqual([]);
    expect(root.getAttribute("data-fonts")).toBe("bundled");
    expect(warn).not.toHaveBeenCalled();
    bootstrap.stop();
  });

  test("missing font files warn once and fall back without stopping launch", async () => {
    const root = document.createElement("div");
    const warn = vi.fn();
    const bootstrap = bootstrapFonts(root, "dark", {
      store: memoryScaleStore(),
      window: densityWindow(1.5),
      fontFaceSet: fontSet([]),
      warn,
    });

    expect(root.getAttribute("data-density")).toBe("1.5x");
    await bootstrap.fonts;
    expect(root.getAttribute("data-fonts")).toBe("fallback");
    expect(warn).toHaveBeenCalledTimes(1);
    bootstrap.stop();
  });

  test("an unverifiable webview still starts and is labelled unknown", async () => {
    const root = document.createElement("div");
    const bootstrap = bootstrapFonts(root, "dark", {
      store: memoryScaleStore(),
      window: densityWindow(1),
      fontFaceSet: null,
    });

    await bootstrap.fonts;
    expect(root.getAttribute("data-fonts")).toBe("unknown");
    expect(root.getAttribute("data-font-scale")).toBe("100");
    bootstrap.stop();
  });

  test("the controller applies and persists a new scale", () => {
    const root = document.createElement("div");
    const store = memoryScaleStore();
    const bootstrap = bootstrapFonts(root, "dark", {
      store,
      window: densityWindow(2),
      fontFaceSet: null,
    });

    bootstrap.scale.set(110);
    expect(store.stored()).toBe("110");
    expect(root.getAttribute("data-font-scale")).toBe("110");
    expect(root.style.getPropertyValue("--at-text-16")).toBe("18px");
    bootstrap.stop();
  });
});
