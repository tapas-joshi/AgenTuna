import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Checkbox } from "./checkbox";

afterEach(cleanup);

describe("Checkbox", () => {
  test("exposes role checkbox, aria-checked and a visible label", () => {
    render(<Checkbox label="Select all" checked onCheckedChange={() => undefined} />);
    const checkbox = screen.getByRole("checkbox", { name: "Select all" });

    expect(checkbox.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByLabelText("Select all")).toBe(checkbox);
  });

  test("indeterminate renders aria-checked=mixed and the mixed mark", () => {
    render(<Checkbox label="Select all" checked="indeterminate" onCheckedChange={() => undefined} />);
    const checkbox = screen.getByRole("checkbox", { name: "Select all" });

    expect(checkbox.getAttribute("aria-checked")).toBe("mixed");
    expect(checkbox.querySelector("[data-slot='checkbox-indicator']")).not.toBeNull();
  });

  test("click reports the next state and stays controlled", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Checkbox label="Select all" checked onCheckedChange={onCheckedChange} />);
    const checkbox = screen.getByRole("checkbox", { name: "Select all" });

    await user.click(checkbox);

    expect(onCheckedChange).toHaveBeenCalledWith(false);
    expect(checkbox.getAttribute("aria-checked")).toBe("true");
  });

  test("Space and Enter both toggle once", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Checkbox label="Select all" checked onCheckedChange={onCheckedChange} />);

    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("checkbox", { name: "Select all" }));

    await user.keyboard(" ");
    expect(onCheckedChange).toHaveBeenCalledTimes(1);
    expect(onCheckedChange).toHaveBeenLastCalledWith(false);

    await user.keyboard("{Enter}");
    expect(onCheckedChange).toHaveBeenCalledTimes(2);
    expect(onCheckedChange).toHaveBeenLastCalledWith(false);
  });

  test("an unlabelled checkbox requires an aria-label and still has a name", () => {
    render(<Checkbox aria-label="Select row" checked={false} onCheckedChange={() => undefined} />);
    expect(screen.getByRole("checkbox", { name: "Select row" })).toBeTruthy();
  });

  test("description is linked through aria-describedby", () => {
    render(
      <Checkbox
        label="Select all"
        description="Applies to every agent."
        checked={false}
        onCheckedChange={() => undefined}
      />,
    );
    const checkbox = screen.getByRole("checkbox", { name: "Select all" });
    const describedBy = checkbox.getAttribute("aria-describedby");

    expect(document.getElementById(describedBy ?? "")?.textContent).toBe("Applies to every agent.");
  });

  test("disabled checkboxes cannot be activated and leave the tab order", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Checkbox label="Disabled" checked={false} onCheckedChange={onCheckedChange} disabled />);
    const checkbox = screen.getByRole("checkbox", { name: "Disabled" }) as HTMLButtonElement;

    await user.click(checkbox);
    await user.tab();

    expect(checkbox.disabled).toBe(true);
    expect(onCheckedChange).not.toHaveBeenCalled();
    expect(document.activeElement).not.toBe(checkbox);
  });
});
