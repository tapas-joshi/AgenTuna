import { describe, expect, test } from "vitest";
import { applyDensity, pixelRatioBucket, watchDensity, type DensityWindow } from "./density";

interface FakeWindow extends DensityWindow {
  devicePixelRatio: number;
  readonly queries: string[];
  fire(): void;
  listenerCount(): number;
}

function fakeWindow(ratio: number): FakeWindow {
  const listeners = new Set<() => void>();
  const queries: string[] = [];
  return {
    devicePixelRatio: ratio,
    queries,
    matchMedia(query: string) {
      queries.push(query);
      return {
        addEventListener: (_type: "change", listener: () => void) => {
          listeners.add(listener);
        },
        removeEventListener: (_type: "change", listener: () => void) => {
          listeners.delete(listener);
        },
      };
    },
    fire: () => {
      for (const listener of [...listeners]) {
        listener();
      }
    },
    listenerCount: () => listeners.size,
  };
}

describe("density buckets", () => {
  test("maps the screenshot matrix onto 1x, 1.5x and 2x", () => {
    expect(pixelRatioBucket(1)).toBe("1x");
    expect(pixelRatioBucket(1.1)).toBe("1x");
    expect(pixelRatioBucket(1.25)).toBe("1.5x");
    expect(pixelRatioBucket(1.5)).toBe("1.5x");
    expect(pixelRatioBucket(1.75)).toBe("2x");
    expect(pixelRatioBucket(2)).toBe("2x");
    expect(pixelRatioBucket(3)).toBe("2x");
  });

  test("writes the attribute and the pixel ratio variable", () => {
    const root = document.createElement("div");
    const info = applyDensity(root, 1.5);
    expect(info).toEqual({ pixelRatio: 1.5, bucket: "1.5x" });
    expect(root.getAttribute("data-density")).toBe("1.5x");
    expect(root.style.getPropertyValue("--at-pixel-ratio")).toBe("1.5");
  });

  test("falls back to 1x for an unusable ratio", () => {
    const root = document.createElement("div");
    expect(applyDensity(root, Number.NaN).bucket).toBe("1x");
    expect(applyDensity(root, 0).pixelRatio).toBe(1);
  });
});

describe("density tracking", () => {
  test("applies the startup ratio and follows monitor changes", () => {
    const root = document.createElement("div");
    const target = fakeWindow(2);
    const controller = watchDensity(root, target);

    expect(root.getAttribute("data-density")).toBe("2x");
    expect(root.style.getPropertyValue("--at-pixel-ratio")).toBe("2");
    expect(controller.info.bucket).toBe("2x");
    expect(target.queries).toEqual(["(resolution: 2dppx)"]);

    target.devicePixelRatio = 1.5;
    target.fire();
    expect(root.getAttribute("data-density")).toBe("1.5x");
    expect(controller.info.pixelRatio).toBe(1.5);
    expect(target.queries).toEqual(["(resolution: 2dppx)", "(resolution: 1.5dppx)"]);
    expect(target.listenerCount()).toBe(1);

    target.devicePixelRatio = 1;
    target.fire();
    expect(root.getAttribute("data-density")).toBe("1x");
  });

  test("stop detaches the listener and leaves the last state", () => {
    const root = document.createElement("div");
    const target = fakeWindow(2);
    const controller = watchDensity(root, target);
    controller.stop();
    expect(target.listenerCount()).toBe(0);

    target.devicePixelRatio = 1;
    target.fire();
    expect(root.getAttribute("data-density")).toBe("2x");
  });

  test("still applies the startup value without matchMedia", () => {
    const root = document.createElement("div");
    const controller = watchDensity(root, { devicePixelRatio: 2 });
    expect(root.getAttribute("data-density")).toBe("2x");
    controller.stop();
  });
});
