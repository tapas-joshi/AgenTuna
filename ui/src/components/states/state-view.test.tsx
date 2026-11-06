import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { StateView } from "./state-view";
import type { ClipboardPort, StateVariant } from "./types";

afterEach(cleanup);

function stubClipboard(fail = false): ClipboardPort & { readonly writes: string[] } {
  const writes: string[] = [];
  return {
    writes,
    writeText: async (text: string) => {
      if (fail) {
        throw new Error("clipboard denied");
      }
      writes.push(text);
    },
  };
}

describe("StateView", () => {
  test("renders title, body and both actions through typed callbacks", async () => {
    const user = userEvent.setup();
    const primary = vi.fn();
    const secondary = vi.fn();
    render(
      <StateView
        variant="empty"
        title="No skills in this scope"
        body="Import a skill or create one from scratch."
        actions={[
          { label: "Add skill", onClick: primary },
          { label: "Import from git", onClick: secondary },
        ]}
      />,
    );

    expect(screen.getByRole("heading", { name: "No skills in this scope" })).toBeTruthy();
    expect(screen.getByText("Import a skill or create one from scratch.")).toBeTruthy();

    const add = screen.getByRole("button", { name: "Add skill" });
    const importButton = screen.getByRole("button", { name: "Import from git" });
    expect(add.getAttribute("data-variant")).toBe("primary");
    expect(importButton.getAttribute("data-variant")).toBe("secondary");

    await user.click(add);
    await user.click(importButton);

    expect(primary).toHaveBeenCalledTimes(1);
    expect(secondary).toHaveBeenCalledTimes(1);
  });

  test("loading shows a labelled spinner and marks the region busy", () => {
    render(<StateView variant="loading" title="Loading skills…" />);

    const root = document.querySelector('[data-slot="state-view"]');
    expect(root?.getAttribute("aria-busy")).toBe("true");
    expect(root?.getAttribute("data-variant")).toBe("loading");
    expect(screen.getByRole("progressbar", { name: "Loading skills…" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Loading skills…" })).toBeTruthy();
  });

  test("skeleton rows replace the spinner and stay out of the accessibility tree", () => {
    render(<StateView variant="loading" title="Loading skills…" skeletonRows={4} />);

    expect(screen.queryByRole("progressbar")).toBeNull();
    const rows = document.querySelectorAll('[data-slot="skeleton-row"]');
    expect(rows).toHaveLength(4);
    const skeleton = document.querySelector('[data-slot="skeleton"]');
    expect(skeleton?.getAttribute("aria-hidden")).toBe("true");
  });

  test("live region is polite by default and assertive for errors", () => {
    const { rerender } = render(<StateView variant="empty" title="No agents detected" />);
    const polite = screen.getByRole("status");
    expect(polite.getAttribute("aria-live")).toBe("polite");
    expect(polite.textContent).toBe("Nothing here yet: No agents detected");

    rerender(<StateView variant="error" title="Agent detection failed" />);
    const assertive = screen.getByRole("alert");
    expect(assertive.getAttribute("aria-live")).toBe("assertive");

    rerender(<StateView variant="loading" title="Detecting agents…" ariaLive="off" />);
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  test("details are collapsed by default and expand on demand", async () => {
    const user = userEvent.setup();
    const clipboard = stubClipboard();
    render(
      <StateView
        variant="error"
        title="Couldn't load skills"
        details={{ summary: "Technical details (E_IO)", text: "path: /tmp/skills\nmessage: I/O error" }}
        clipboard={clipboard}
      />,
    );

    const disclosure = screen.getByRole("button", { name: "Technical details (E_IO)" });
    expect(disclosure.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText(/path: \/tmp\/skills/)).toBeNull();

    await user.click(disclosure);

    expect(disclosure.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText(/path: \/tmp\/skills/)).toBeTruthy();
    expect(screen.getByText(/message: I\/O error/)).toBeTruthy();
  });

  test("Copy details writes the exact payload and reports success", async () => {
    const user = userEvent.setup();
    const clipboard = stubClipboard();
    render(
      <StateView
        variant="error"
        title="Couldn't load skills"
        details={{ text: "code: E_IO\npath: /tmp/skills" }}
        clipboard={clipboard}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Technical details" }));
    await user.click(screen.getByRole("button", { name: "Copy details" }));

    expect(clipboard.writes).toEqual(["code: E_IO\npath: /tmp/skills"]);
    expect(screen.getByRole("button", { name: "Copied" })).toBeTruthy();
    expect(screen.getByRole("status").textContent).toBe("Details copied");
  });

  test("a failed copy is surfaced instead of pretending it worked", async () => {
    const user = userEvent.setup();
    render(
      <StateView
        variant="error"
        title="Couldn't load skills"
        details={{ text: "code: E_IO" }}
        clipboard={stubClipboard(true)}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Technical details" }));
    await user.click(screen.getByRole("button", { name: "Copy details" }));

    expect(screen.getByRole("button", { name: "Copy failed" })).toBeTruthy();
  });

  test("minHeight reserves layout space without raw tokens", () => {
    render(<StateView variant="empty" title="No rows" minHeight={320} />);

    const root = document.querySelector<HTMLElement>('[data-slot="state-view"]');
    expect(root?.style.minHeight).toBe("320px");
    expect(root?.className).not.toMatch(/#[0-9a-f]{3,8}/i);
  });

  test.each(["loading", "empty", "error", "offline"] satisfies StateVariant[])(
    "%s paints with theme token classes only",
    (variant) => {
      render(<StateView variant={variant} title="State" />);
      const markup = document.body.innerHTML;
      expect(markup).not.toMatch(/#[0-9a-f]{3,8}\b/i);
      expect(markup).not.toMatch(/text-\[[^\]]*px[^\]]*\]/);
      const heading = screen.getByRole("heading", { name: "State" });
      expect(heading.className).toContain("text-text");
    },
  );
});
