import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { applyTheme } from "../../theme";
import { DiffViewer } from "./diff-viewer";
import { MAX_DIFF_BYTES, MAX_DIFF_LINES } from "./model";

afterEach(() => {
  cleanup();
  applyTheme(document.documentElement, "dark");
});

const SAMPLE = [
  "--- a/skills/demo/SKILL.md",
  "+++ b/skills/demo/SKILL.md",
  "@@ -1,4 +1,5 @@",
  " # Demo",
  "-old line",
  "+new line",
  "-same   ",
  "+same",
  "+extra",
  " context",
].join("\n");

describe("DiffViewer", () => {
  test("renders unified line numbers, +/- counts and textual markers", () => {
    const { container } = render(<DiffViewer diff={SAMPLE} />);

    expect(screen.getByText("skills/demo/SKILL.md")).toBeTruthy();
    expect(screen.getByLabelText("3 additions, 2 deletions")).toBeTruthy();

    const added = container.querySelector<HTMLElement>('[data-slot="diff-line"][data-kind="add"]');
    const removed = container.querySelector<HTMLElement>('[data-slot="diff-line"][data-kind="remove"]');
    expect(added?.textContent).toContain("+new line");
    expect(removed?.textContent).toContain("-old line");
    expect(added?.querySelector('[data-kind="add"]')?.className).toContain("bg-success/10");
    expect(removed?.querySelector('[data-kind="remove"]')?.className).toContain("bg-danger/10");

    // Both number columns are present with the source line numbers.
    expect(screen.getByText("old line").closest('[data-slot="diff-line"]')?.textContent).toContain("2");
  });

  test.each(["dark", "light"] as const)(
    "keeps success/danger token classes and textual markers in the %s theme",
    (mode) => {
      applyTheme(document.documentElement, mode);
      const { container } = render(<DiffViewer diff={SAMPLE} />);

      const added = container.querySelector<HTMLElement>('[data-slot="diff-line"][data-kind="add"]');
      const removed = container.querySelector<HTMLElement>(
        '[data-slot="diff-line"][data-kind="remove"]',
      );

      expect(added?.textContent).toContain("+new line");
      expect(removed?.textContent).toContain("-old line");
      expect(added?.querySelector('[data-kind="add"]')?.className).toContain("bg-success/10");
      expect(removed?.querySelector('[data-kind="remove"]')?.className).toContain("bg-danger/10");
      expect(document.documentElement.getAttribute("data-theme")).toBe(mode);
    },
  );

  test("renders side-by-side rows from the same model with both side numbers", async () => {
    const user = userEvent.setup();
    const { container } = render(<DiffViewer diff={SAMPLE} />);

    await user.click(screen.getByRole("radio", { name: "Side by side" }));

    const rows = container.querySelectorAll('[data-slot="diff-row"]');
    expect(rows).toHaveLength(5);
    const replaced = Array.from(rows).find((row) => row.textContent?.includes("old line"));
    expect(replaced?.textContent).toContain("new line");
    expect(replaced?.children).toHaveLength(6);
    expect(replaced?.children[0]?.textContent).toBe("2");
    expect(replaced?.children[3]?.textContent).toBe("2");
    expect(replaced?.querySelector('[data-kind="remove"]')).toBeTruthy();
    expect(replaced?.querySelector('[data-kind="add"]')).toBeTruthy();
    expect(screen.getByLabelText("3 additions, 2 deletions")).toBeTruthy();
  });

  test("hide whitespace updates counts and removes only formatting-only pairs", async () => {
    const user = userEvent.setup();
    const { container } = render(<DiffViewer diff={SAMPLE} />);

    expect(container.querySelectorAll('[data-slot="diff-line"][data-kind="add"]')).toHaveLength(3);
    expect(container.querySelectorAll('[data-slot="diff-line"][data-kind="remove"]')).toHaveLength(2);

    await user.click(screen.getByRole("switch", { name: "Hide whitespace" }));

    expect(screen.getByLabelText("2 additions, 1 deletions")).toBeTruthy();
    expect(screen.queryByText("same")).toBeNull();
    expect(screen.queryByText("same   ")).toBeNull();
    expect(container.querySelectorAll('[data-slot="diff-line"][data-kind="add"]')).toHaveLength(2);
    expect(container.querySelectorAll('[data-slot="diff-line"][data-kind="remove"]')).toHaveLength(1);
  });

  test("disables the whitespace switch when no pair differs only by spacing", () => {
    render(<DiffViewer diff={"--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b"} />);

    expect(screen.getByRole("switch", { name: "Hide whitespace" }).hasAttribute("disabled")).toBe(true);
  });

  test("Copy diff passes the full patch to the injected writer", async () => {
    const user = userEvent.setup();
    const copy = vi.fn();
    render(<DiffViewer diff={SAMPLE} copy={copy} />);

    await user.click(screen.getByRole("button", { name: "Copy diff" }));

    await waitFor(() => expect(copy).toHaveBeenCalledWith(SAMPLE));
    expect(screen.getByRole("button", { name: "Copied" })).toBeTruthy();
  });

  test("reports a clipboard failure instead of pretending success", async () => {
    const user = userEvent.setup();
    render(<DiffViewer diff={SAMPLE} copy={() => Promise.reject(new Error("denied"))} />);

    await user.click(screen.getByRole("button", { name: "Copy diff" }));

    await waitFor(() => expect(screen.getByRole("button", { name: "Copy failed" })).toBeTruthy());
  });

  test("truncates oversized input with a notice but still copies the full patch", async () => {
    const user = userEvent.setup();
    const copy = vi.fn();
    const oversized = "x".repeat(MAX_DIFF_BYTES + 1);
    render(<DiffViewer diff={oversized} copy={copy} />);

    const notice = screen.getByRole("status");
    expect(notice.textContent).toContain("Diff truncated");
    expect(notice.textContent).toContain("2 MB");

    await user.click(screen.getByRole("button", { name: "Copy diff" }));

    await waitFor(() => expect(copy).toHaveBeenCalledWith(oversized));
  });

  test(
    "truncates diffs over 20,000 lines with a notice",
    () => {
      const body = Array.from({ length: MAX_DIFF_LINES }, (_, index) => ` line ${index}`).join("\n");
      const diff = `--- a/x\n+++ b/x\n@@ -1,${MAX_DIFF_LINES} +1,${MAX_DIFF_LINES} @@\n${body}`;

      render(<DiffViewer diff={diff} />);

      const notice = screen.getByRole("status");
      expect(notice.textContent).toContain("20,000 lines");
      expect(notice.textContent).toContain("Copy diff");
    },
    20_000,
  );

  test("renders a binary note and an empty diff without pretending there are lines", () => {
    const binary = render(<DiffViewer diff="Binary files a/logo.png differ" />);
    expect(screen.getByText(/Binary file/)).toBeTruthy();
    expect(screen.getByLabelText("0 additions, 0 deletions")).toBeTruthy();
    binary.unmount();

    render(<DiffViewer diff="" />);
    expect(screen.getByText("No changes.")).toBeTruthy();
  });

  test("renders multiple files with their own headings", () => {
    const { container } = render(
      <DiffViewer
        diff={[
          "--- a/one",
          "+++ b/one",
          "@@ -1 +1 @@",
          "-a",
          "+b",
          "--- a/two",
          "+++ b/two",
          "@@ -1 +1 @@",
          "-c",
          "+d",
        ].join("\n")}
      />,
    );

    const headings = Array.from(container.querySelectorAll('[data-slot="diff-body"] > div > div')).map(
      (element) => element.textContent,
    );
    expect(headings).toContain("one");
    expect(headings).toContain("two");
  });
});

describe("DiffViewer accessibility", () => {
  test("keeps counts and modes reachable by role and name", async () => {
    const user = userEvent.setup();
    render(<DiffViewer diff={SAMPLE} />);

    const group = screen.getByRole("radiogroup", { name: "Diff layout" });
    expect(within(group).getAllByRole("radio")).toHaveLength(2);
    expect(screen.getByRole("switch", { name: "Hide whitespace" })).toBeTruthy();
    expect(screen.getByRole("region", { name: "Diff of skills/demo/SKILL.md" })).toBeTruthy();

    await user.click(within(group).getByRole("radio", { name: "Unified" }));
    expect(screen.getByRole("radio", { name: "Unified" }).getAttribute("aria-checked")).toBe("true");
  });
});
