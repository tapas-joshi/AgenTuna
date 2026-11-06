import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";
import { Skeleton } from "./skeleton";
import { Spinner } from "./spinner";

afterEach(cleanup);

describe("Spinner", () => {
  test("is a named progressbar drawn with tokens", () => {
    render(<Spinner label="Loading skills…" />);

    const spinner = screen.getByRole("progressbar", { name: "Loading skills…" });
    expect(spinner.className).toContain("border-t-accent");
    expect(spinner.className).toContain("animate-spin");
    expect(spinner.className).not.toMatch(/#[0-9a-f]{3,8}/i);
  });

  test("sizes map to the spacing tokens", () => {
    const { rerender } = render(<Spinner label="Loading" size="sm" />);
    expect(screen.getByRole("progressbar").className).toContain("size-12");

    rerender(<Spinner label="Loading" size="md" />);
    expect(screen.getByRole("progressbar").className).toContain("size-24");
  });
});

describe("Skeleton", () => {
  test("renders the requested number of decorative rows", () => {
    render(<Skeleton rows={5} />);

    const skeleton = document.querySelector('[data-slot="skeleton"]');
    expect(skeleton?.getAttribute("aria-hidden")).toBe("true");
    expect(document.querySelectorAll('[data-slot="skeleton-row"]')).toHaveLength(5);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  test("defaults to a single row", () => {
    render(<Skeleton />);

    expect(document.querySelectorAll('[data-slot="skeleton-row"]')).toHaveLength(1);
  });
});
