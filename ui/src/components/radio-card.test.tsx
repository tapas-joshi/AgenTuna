import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { RadioCardGroup, type RadioCardOption } from "./radio-card";

afterEach(cleanup);

type Placement = "replace" | "watch" | "skip";

const OPTIONS: readonly RadioCardOption<Placement>[] = [
  { value: "replace", title: "Replace the original", description: "Leave a link in the agent folder." },
  { value: "watch", title: "Watch in place", description: "Detect external edits." },
  { value: "skip", title: "Leave unmanaged", description: "Scan only.", disabled: true },
];

function Harness({ onValueChange }: { readonly onValueChange: (value: Placement) => void }) {
  const [value, setValue] = useState<Placement>("replace");
  return (
    <RadioCardGroup
      label="Migration placement"
      value={value}
      onValueChange={(next) => {
        setValue(next);
        onValueChange(next);
      }}
      options={OPTIONS}
      columns={3}
    />
  );
}

describe("RadioCardGroup", () => {
  test("renders a labelled radiogroup with one card per option", () => {
    render(<Harness onValueChange={() => undefined} />);
    const group = screen.getByRole("radiogroup", { name: "Migration placement" });
    const cards = screen.getAllByRole("radio");

    expect(group.className).toContain("grid");
    expect(cards).toHaveLength(3);
    expect(cards[0]?.getAttribute("aria-checked")).toBe("true");
    expect(cards[0]?.getAttribute("data-state")).toBe("checked");
    expect(cards[2]?.hasAttribute("disabled")).toBe(true);
    expect(screen.getByText("Leave a link in the agent folder.")).toBeTruthy();
  });

  test("the title is the accessible name and the description is linked", () => {
    render(<Harness onValueChange={() => undefined} />);
    const card = screen.getByRole("radio", { name: "Watch in place" });
    const describedBy = card.getAttribute("aria-describedby");

    expect(document.getElementById(describedBy ?? "")?.textContent).toBe("Detect external edits.");
  });

  test("Tab reaches the checked card", async () => {
    const user = userEvent.setup();
    render(<Harness onValueChange={() => undefined} />);

    await user.tab();

    expect(document.activeElement).toBe(screen.getByRole("radio", { name: /Replace the original/ }));
  });

  test("click selects a card and reports the value", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);

    await user.click(screen.getByRole("radio", { name: /Watch in place/ }));

    expect(onValueChange).toHaveBeenCalledWith("watch");
    expect(screen.getByRole("radio", { name: /Watch in place/ }).getAttribute("aria-checked")).toBe("true");
  });

  test("arrow keys move the selection", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);

    await user.tab();
    // Focus moves in a following task, so hold the key until it lands.
    await user.keyboard("{ArrowDown>}");
    await user.keyboard("{/ArrowDown}");

    expect(onValueChange).toHaveBeenLastCalledWith("watch");
    expect(screen.getByRole("radio", { name: /Watch in place/ }).getAttribute("aria-checked")).toBe("true");
  });

  test("Enter and Space select the focused card", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);
    const watch = screen.getByRole("radio", { name: /Watch in place/ });

    watch.focus();
    await user.keyboard("{Enter}");
    expect(onValueChange).toHaveBeenNthCalledWith(1, "watch");
    expect(watch.getAttribute("aria-checked")).toBe("true");

    const replace = screen.getByRole("radio", { name: /Replace the original/ });
    replace.focus();
    await user.keyboard(" ");
    expect(onValueChange).toHaveBeenNthCalledWith(2, "replace");
    expect(replace.getAttribute("aria-checked")).toBe("true");
  });

  test("a disabled card cannot be selected", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(<Harness onValueChange={onValueChange} />);

    await user.click(screen.getByRole("radio", { name: /Leave unmanaged/ }));

    expect(onValueChange).not.toHaveBeenCalled();
    expect(screen.getByRole("radio", { name: /Leave unmanaged/ }).getAttribute("aria-checked")).toBe("false");
  });
});
