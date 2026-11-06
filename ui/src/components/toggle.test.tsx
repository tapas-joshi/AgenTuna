import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Toggle } from "./toggle";

afterEach(cleanup);

describe("Toggle", () => {
  test("exposes role switch, aria-checked and the label as its name", () => {
    render(<Toggle label="Linked" checked onCheckedChange={() => undefined} />);
    const toggle = screen.getByRole("switch", { name: "Linked" });

    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByLabelText("Linked")).toBe(toggle);
  });

  test("click reports the next state without flipping itself", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Toggle label="Linked" checked={false} onCheckedChange={onCheckedChange} />);
    const toggle = screen.getByRole("switch", { name: "Linked" });

    await user.click(toggle);

    expect(onCheckedChange).toHaveBeenCalledWith(true);
    expect(toggle.getAttribute("aria-checked")).toBe("false");
  });

  test("Space and Enter toggle the focused switch", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Toggle label="Linked" checked={false} onCheckedChange={onCheckedChange} />);

    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("switch", { name: "Linked" }));

    await user.keyboard(" ");
    await user.keyboard("{Enter}");

    expect(onCheckedChange).toHaveBeenNthCalledWith(1, true);
    expect(onCheckedChange).toHaveBeenNthCalledWith(2, true);
  });

  test("description is linked through aria-describedby", () => {
    render(
      <Toggle
        label="Background health checks"
        description="Runs only for approved servers."
        checked={false}
        onCheckedChange={() => undefined}
      />,
    );
    const toggle = screen.getByRole("switch", { name: "Background health checks" });
    const describedBy = toggle.getAttribute("aria-describedby");

    expect(describedBy).not.toBeNull();
    expect(document.getElementById(describedBy ?? "")?.textContent).toBe("Runs only for approved servers.");
  });

  test("label placement below keeps the same accessible name", () => {
    render(<Toggle label="Linked" labelPlacement="below" checked onCheckedChange={() => undefined} />);
    expect(screen.getByRole("switch", { name: "Linked" })).toBeTruthy();
  });

  test("disabled switches are removed from Tab and do not activate", async () => {
    const user = userEvent.setup();
    const onCheckedChange = vi.fn();
    render(<Toggle label="Locked" checked={false} onCheckedChange={onCheckedChange} disabled />);
    const toggle = screen.getByRole("switch", { name: "Locked" }) as HTMLButtonElement;

    await user.click(toggle);
    await user.tab();

    expect(toggle.disabled).toBe(true);
    expect(onCheckedChange).not.toHaveBeenCalled();
    expect(document.activeElement).not.toBe(toggle);
  });
});
