import { afterEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { NativeBanner } from "./NativeBanner";

describe("NativeBanner", () => {
  afterEach(() => {
    cleanup();
  });

  test("renders nothing when count is 0", () => {
    const { container } = render(
      <NativeBanner
        kind="skill"
        count={0}
        onReviewMigrate={vi.fn()}
        onHide={vi.fn()}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  test("renders exact count and plural noun for multiple skills", () => {
    render(
      <NativeBanner
        kind="skill"
        count={3}
        onReviewMigrate={vi.fn()}
        onHide={vi.fn()}
      />,
    );
    expect(
      screen.getByText("3 skills in agent folders are not in AgenTuna"),
    ).toBeTruthy();
  });

  test("renders singular phrasing for single MCP server", () => {
    render(
      <NativeBanner
        kind="mcp_server"
        count={1}
        onReviewMigrate={vi.fn()}
        onHide={vi.fn()}
      />,
    );
    expect(
      screen.getByText("1 MCP server in agent folders is not in AgenTuna"),
    ).toBeTruthy();
  });

  test("calls onReviewMigrate and onHide on button clicks", async () => {
    const user = userEvent.setup();
    const onReview = vi.fn();
    const onHide = vi.fn();

    render(
      <NativeBanner
        kind="skill"
        count={5}
        onReviewMigrate={onReview}
        onHide={onHide}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Review & migrate" }));
    expect(onReview).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "Hide" }));
    expect(onHide).toHaveBeenCalledTimes(1);
  });
});
