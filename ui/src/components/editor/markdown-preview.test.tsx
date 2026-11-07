import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { isSafeExternalUrl } from "./external-link";
import { MarkdownPreview } from "./markdown-preview";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("MarkdownPreview external links", () => {
  test("an https link opens through the typed command instead of navigating the webview", () => {
    invoke.mockResolvedValue(null);
    const origin = window.location.href;
    render(<MarkdownPreview markdown="See [the guide](https://example.com/guide?x=1)." />);

    const link = screen.getByRole("link", { name: "the guide" });
    expect(link.getAttribute("target")).toBeNull();
    expect(link.getAttribute("href")).toBe("https://example.com/guide?x=1");
    expect(link.getAttribute("title")).toBe("https://example.com/guide?x=1");

    // `false` means `preventDefault` ran, so the webview would not start a navigation.
    expect(fireEvent.click(link)).toBe(false);
    expect(window.location.href).toBe(origin);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("open_external_link", { url: "https://example.com/guide?x=1" });
  });

  test("Enter on a focused link opens it exactly once and cancels the navigation", () => {
    invoke.mockResolvedValue(null);
    render(<MarkdownPreview markdown="[the guide](https://example.com/guide)" />);

    const link = screen.getByRole("link", { name: "the guide" });
    expect(fireEvent.keyDown(link, { key: "Enter" })).toBe(false);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("open_external_link", { url: "https://example.com/guide" });
  });

  test("javascript: and relative targets render as text and never reach the command", () => {
    render(
      <MarkdownPreview markdown={"- [script](javascript:alert(1))\n- [relative](./docs/SKILL.md)"} />,
    );

    expect(screen.queryByRole("link")).toBeNull();
    const script = screen.getByText("script");
    const relative = screen.getByText("relative");
    expect(fireEvent.click(script)).toBe(true);
    expect(fireEvent.click(relative)).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  test("the shared validator mirrors the backend rules", () => {
    for (const accepted of ["https://example.com", "https://sub.example.co.uk/a?b=1#c", "https://例え.jp/パス"]) {
      expect(isSafeExternalUrl(accepted)).toBe(true);
    }
    for (const rejected of [
      "http://example.com",
      "javascript:alert(1)",
      "file:///etc/passwd",
      "data:text/html,<p>x</p>",
      "https://user:password@example.com/",
      "./relative",
      "docs/SKILL.md",
      "",
      `https://example.com/${"a".repeat(2048)}`,
      "https://example.com/\u0007bell",
    ]) {
      expect(isSafeExternalUrl(rejected)).toBe(false);
    }
    expect(isSafeExternalUrl(undefined)).toBe(false);
  });
});
