import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { CommandError } from "../../generated/CommandError";
import { ERROR_COPY, errorDetailsText, toErrorReport } from "./error-model";
import { ErrorState } from "./error-state";
import type { ClipboardPort } from "./types";

afterEach(cleanup);

const PATH = "/Users/ana/.claude/skills/tdd/SKILL.md";

const PERMISSION_REPORT = {
  code: "E_PERMISSION_DENIED",
  message: "Permission denied (os error 13)",
  user_message: `Cannot write ${PATH} because the folder is read-only.`,
  path: PATH,
  operation_id: "op_01J8Z6R2K3M4N5P6Q7R8S9T0V1",
  source_chain: [`failed to open ${PATH}`, "Permission denied"],
  remedy: ["reveal_in_finder", "copy_details"],
  retryable: false,
};

const NETWORK_REPORT = {
  code: "E_NETWORK",
  message: "Connection refused (os error 61)",
  path: undefined,
  source_chain: [],
  remedy: ["retry", "open_settings", "copy_details"],
  retryable: true,
};

function stubClipboard(): ClipboardPort & { readonly writes: string[] } {
  const writes: string[] = [];
  return {
    writes,
    writeText: async (text: string) => {
      writes.push(text);
    },
  };
}

function reportWith(code: string, extra: Record<string, unknown> = {}) {
  return toErrorReport({ code, message: `${code} technical text`, ...extra });
}

describe("ErrorState", () => {
  test("shows human copy with the raw path only in collapsed details", async () => {
    const user = userEvent.setup();
    const clipboard = stubClipboard();
    const onRemedy = vi.fn();
    const report = toErrorReport(PERMISSION_REPORT);

    render(<ErrorState error={report} onRemedy={onRemedy} clipboard={clipboard} />);

    expect(screen.getByRole("heading", { name: "AgenTuna can't open this file" })).toBeTruthy();
    expect(screen.getByText(ERROR_COPY.permission_denied.body)).toBeTruthy();
    expect(screen.queryByText(new RegExp(PATH))).toBeNull();

    const disclosure = screen.getByRole("button", {
      name: "Technical details (E_PERMISSION_DENIED)",
    });
    expect(disclosure.getAttribute("aria-expanded")).toBe("false");

    await user.click(disclosure);

    expect(screen.getByText(new RegExp(`path: ${PATH}`))).toBeTruthy();
    expect(screen.getByText(/message: Permission denied \(os error 13\)/)).toBeTruthy();
    expect(screen.getByText(/operation: op_01J8Z6R2K3M4N5P6Q7R8S9T0V1/)).toBeTruthy();
  });

  test("Copy details keeps the path and the OS error for a bug report", async () => {
    const user = userEvent.setup();
    const clipboard = stubClipboard();
    const report = toErrorReport(PERMISSION_REPORT);

    render(<ErrorState error={report} clipboard={clipboard} />);
    await user.click(screen.getByRole("button", { name: /Technical details/ }));
    await user.click(screen.getByRole("button", { name: "Copy details" }));

    const [copied] = clipboard.writes;
    expect(copied).toBe(errorDetailsText(report));
    expect(copied).toContain(`path: ${PATH}`);
    expect(copied).toContain("Permission denied (os error 13)");
    expect(copied).toContain("caused by:");
  });

  test.each([
    ["E_PERMISSION_DENIED", "AgenTuna can't open this file"],
    ["E_INVALID_FORMAT", "This file isn't valid"],
    ["E_NOT_FOUND", "That item no longer exists"],
    ["E_REFUSED_OVERWRITE", "Something already uses this name"],
    ["E_NETWORK", "You're offline"],
    ["E_INTERNAL", "Something went wrong"],
  ])("maps %s to human copy", (code, title) => {
    render(<ErrorState error={reportWith(code)} />);

    expect(screen.getByRole("heading", { name: title })).toBeTruthy();
    expect(screen.queryByText(/technical text/)).toBeNull();
  });

  test("a missing agent is rendered from an explicit class override", () => {
    render(<ErrorState error={reportWith("E_NOT_FOUND")} errorClass="missing_agent" />);

    expect(screen.getByRole("heading", { name: "That agent isn't installed" })).toBeTruthy();
    expect(screen.getByText(ERROR_COPY.missing_agent.body)).toBeTruthy();
  });

  test("remedies render as buttons in report order and ignore copy_details", async () => {
    const user = userEvent.setup();
    const onRemedy = vi.fn();
    const report = toErrorReport({
      code: "E_REFUSED_OVERWRITE",
      message: "refused",
      remedy: ["adopt", "move_aside", "copy_details"],
    });

    render(<ErrorState error={report} onRemedy={onRemedy} />);

    const buttons = screen
      .getAllByRole("button")
      .map((button) => button.textContent)
      .filter((label) => label !== "Technical details (E_REFUSED_OVERWRITE)");
    expect(buttons).toEqual(["Adopt", "Move aside"]);

    await user.click(screen.getByRole("button", { name: "Adopt" }));
    await user.click(screen.getByRole("button", { name: "Move aside" }));

    expect(onRemedy.mock.calls).toEqual([["adopt"], ["move_aside"]]);
  });

  test("remedy buttons stay hidden when the screen has no handler", () => {
    const report = toErrorReport(PERMISSION_REPORT);

    render(<ErrorState error={report} />);

    expect(screen.queryByRole("button", { name: "Reveal in Finder" })).toBeNull();
  });

  test("retry runs the callback and spins while the error stays visible", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const report = toErrorReport(NETWORK_REPORT);

    const { rerender } = render(<ErrorState error={report} onRetry={onRetry} />);
    const retry = screen.getByRole("button", { name: "Retry" });
    expect(retry.getAttribute("data-variant")).toBe("primary");

    await user.click(retry);
    expect(onRetry).toHaveBeenCalledTimes(1);

    rerender(<ErrorState error={report} onRetry={onRetry} retrying />);
    const spinning = screen.getByRole("button", { name: "Retry" });
    expect(spinning.getAttribute("aria-busy")).toBe("true");
    expect(spinning.hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("heading", { name: "You're offline" })).toBeTruthy();
  });

  test("no retry button for a class that cannot retry", () => {
    const onRetry = vi.fn();

    render(<ErrorState error={toErrorReport(PERMISSION_REPORT)} onRetry={onRetry} />);

    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  test("a generated CommandError renders with the mapped copy", () => {
    const error: CommandError = {
      code: "unavailable",
      message: "the bundled installer could not be resolved",
    };

    render(<ErrorState error={error} onRetry={vi.fn()} />);

    expect(screen.getByRole("heading", { name: "AgenTuna's background service stopped" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
  });

  test("an offline report uses the offline variant and the warning glyph", () => {
    render(<ErrorState error={reportWith("E_NETWORK")} />);

    const root = document.querySelector('[data-slot="state-view"]');
    expect(root?.getAttribute("data-variant")).toBe("offline");
    expect(document.querySelector('[data-glyph="offline"]')).toBeTruthy();
  });

  test("an unknown rejection value degrades to the unknown copy", () => {
    render(<ErrorState error={toErrorReport(undefined)} />);

    expect(screen.getByRole("heading", { name: "Something went wrong" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /Technical details/ })).toBeTruthy();
  });
});
