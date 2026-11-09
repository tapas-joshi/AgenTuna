import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { SegmentedControl, type SegmentedOption } from "./segmented-control";

afterEach(cleanup);

type Segment = "global" | "user" | "project";

const OPTIONS: readonly SegmentedOption<Segment>[] = [
  { value: "global", label: "Global" },
  { value: "user", label: "User" },
  { value: "project", label: "Project" },
];

function Harness({
  onValueChange,
  initial = "project",
}: {
  readonly onValueChange: (value: Segment) => void;
  readonly initial?: Segment;
}) {
  const [value, setValue] = useState<Segment>(initial);
  return (
    <SegmentedControl
      label="Scope"
      value={value}
      onValueChange={(next) => {
        setValue(next);
        onValueChange(next);
      }}
      options={OPTIONS}
    />
  );
}

describe("SegmentedControl", () => {
  test("is a labelled radiogroup with one checked segment", () => {
    render(<Harness onValueChange={() => undefined} />);
    const group = screen.getByRole("radiogroup", { name: "Scope" });
    const radios = screen.getAllByRole("radio");

    expect(group.getAttribute("aria-orientation")).toBe("horizontal");
    expect(radios.map((radio) => radio.textContent)).toEqual(["Global", "User", "Project"]);
    expect(radios.map((radio) => radio.getAttribute("aria-checked"))).toEqual(["false", "false", "true"]);
    expect(radios[0]?.className).toContain("optical-label-center");
    expect(radios[0]?.className).toContain("h-32");
  });

  test("Tab reaches the group and the checked segment owns the tab stop", async () => {
    const user = userEvent.setup();
    render(<Harness onValueChange={() => undefined} />);

    await user.tab();

    expect(document.activeElement).toBe(screen.getByRole("radio", { name: "Project" }));
  });

  test("click selects a segment and reports the value", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);

    await user.click(screen.getByRole("radio", { name: "User" }));

    expect(onValueChange).toHaveBeenCalledWith("user");
    expect(screen.getByRole("radio", { name: "User" }).getAttribute("aria-checked")).toBe("true");
  });

  test("arrow keys move and select, wrapping at the end", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);

    await user.tab();
    // Radix moves focus in a following task and only selects when the arrow
    // key is still down, so hold it for the duration of the move.
    await user.keyboard("{ArrowRight>}");
    await user.keyboard("{/ArrowRight}");

    expect(onValueChange).toHaveBeenLastCalledWith("global");
    expect(screen.getByRole("radio", { name: "Global" }).getAttribute("aria-checked")).toBe("true");
  });

  test("Enter selects the focused but unchecked segment", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    const userSegment = screen.getByRole("radio", { name: "User" });

    userSegment.focus();
    await user.keyboard("{Enter}");

    expect(onValueChange).toHaveBeenCalledWith("user");
    expect(userSegment.getAttribute("aria-checked")).toBe("true");
  });

  test("Space selects the focused but unchecked segment", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    const globalSegment = screen.getByRole("radio", { name: "Global" });

    globalSegment.focus();
    await user.keyboard(" ");

    expect(onValueChange).toHaveBeenCalledWith("global");
  });

  test("a disabled group cannot be changed or focused", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(
      <SegmentedControl
        label="Scope"
        value="user"
        onValueChange={onValueChange}
        options={OPTIONS}
        disabled
      />,
    );

    await user.click(screen.getByRole("radio", { name: "Project" }));
    await user.tab();

    expect(onValueChange).not.toHaveBeenCalled();
    expect(document.activeElement).not.toBe(screen.getByRole("radio", { name: "User" }));
  });
});
