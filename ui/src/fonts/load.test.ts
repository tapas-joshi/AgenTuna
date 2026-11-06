import { describe, expect, test, vi } from "vitest";
import { ensureFonts, REQUIRED_FONT_FAMILIES, type FontFaceSetLike } from "./load";

function fontSet(available: readonly string[]): FontFaceSetLike {
  return {
    ready: Promise.resolve(),
    load: vi.fn(async () => []),
    check: (descriptor: string) =>
      available.some((family) => descriptor.includes(`"${family}"`)),
  };
}

describe("font availability check", () => {
  test("probes both bundled families", () => {
    expect(REQUIRED_FONT_FAMILIES.map((spec) => spec.family)).toEqual([
      "Inter",
      "JetBrains Mono",
    ]);
  });

  test("reports both families as loaded when the faces resolve", async () => {
    const warn = vi.fn();
    const result = await ensureFonts({ fontFaceSet: fontSet(["Inter", "JetBrains Mono"]), warn });

    expect(result).toEqual({ checked: true, loaded: ["Inter", "JetBrains Mono"], missing: [] });
    expect(warn).not.toHaveBeenCalled();
  });

  test("warns once and names the fallback when a file is missing", async () => {
    const warn = vi.fn();
    const result = await ensureFonts({ fontFaceSet: fontSet(["JetBrains Mono"]), warn });

    expect(result.checked).toBe(true);
    expect(result.missing).toEqual(["Inter"]);
    expect(result.loaded).toEqual(["JetBrains Mono"]);
    expect(warn).toHaveBeenCalledTimes(1);
    const message = warn.mock.calls[0]?.[0] ?? "";
    expect(message).toContain("Inter");
    expect(message).toContain("Helvetica/SF");
    expect(message).not.toContain("JetBrains Mono");
  });

  test("does not warn when the webview cannot verify fonts", async () => {
    const warn = vi.fn();
    const result = await ensureFonts({ fontFaceSet: null, warn });
    expect(result).toEqual({ checked: false, loaded: [], missing: [] });
    expect(warn).not.toHaveBeenCalled();
  });

  test("a rejected load is decided by check, not by the rejection", async () => {
    const warn = vi.fn();
    const failing: FontFaceSetLike = {
      ready: Promise.reject(new Error("network")),
      load: () => Promise.reject(new Error("missing file")),
      check: () => true,
    };
    const result = await ensureFonts({ fontFaceSet: failing, warn });

    expect(result.loaded).toEqual(["Inter", "JetBrains Mono"]);
    expect(warn).not.toHaveBeenCalled();
  });
});
