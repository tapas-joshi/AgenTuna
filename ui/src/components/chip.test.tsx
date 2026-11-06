import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Chip } from "./chip";

afterEach(cleanup);

describe("Chip", () => {
  test("is a two-state button with aria-pressed and a count", async () => {
    const user = userEvent.setup();
    const onPressedChange = vi.fn();
    render(
      <Chip pressed={false} onPressedChange={onPressedChange} count={12} dotTone="scope-user">
        User
      </Chip>,
    );
    const chip = screen.getByRole("button", { name: /User/ });

    expect(chip.getAttribute("aria-pressed")).toBe("false");
    expect(chip.textContent).toContain("12");
    expect(chip.querySelector("[aria-hidden='true']")?.className).toContain("bg-scope-user");

    await user.click(chip);

    expect(onPressedChange).toHaveBeenCalledWith(true);
    // Fully controlled: the chip never flips its own aria-pressed.
    expect(chip.getAttribute("aria-pressed")).toBe("false");
  });

  test("reflects the pressed prop through aria-pressed and data-state", () => {
    render(
      <Chip pressed onPressedChange={() => undefined}>
        All
      </Chip>,
    );
    const chip = screen.getByRole("button", { name: "All" });

    expect(chip.getAttribute("aria-pressed")).toBe("true");
    expect(chip.dataset["state"]).toBe("on");
    expect(chip.className).toContain("data-[state=on]:border-text");
  });

  test("Space and Enter activate the focused chip", async () => {
    const user = userEvent.setup();
    const onPressedChange = vi.fn();
    render(
      <Chip pressed={false} onPressedChange={onPressedChange}>
        Project
      </Chip>,
    );
    const chip = screen.getByRole("button", { name: "Project" });

    chip.focus();
    await user.keyboard(" ");
    await user.keyboard("{Enter}");

    expect(onPressedChange).toHaveBeenNthCalledWith(1, true);
    expect(onPressedChange).toHaveBeenNthCalledWith(2, true);
  });

  test("disabled chips are skipped by Tab and do not activate", async () => {
    const user = userEvent.setup();
    const onPressedChange = vi.fn();
    render(
      <Chip disabled pressed={false} onPressedChange={onPressedChange}>
        Disabled
      </Chip>,
    );
    const chip = screen.getByRole("button", { name: "Disabled" }) as HTMLButtonElement;

    await user.click(chip);
    await user.tab();

    expect(chip.disabled).toBe(true);
    expect(onPressedChange).not.toHaveBeenCalled();
    expect(document.activeElement).not.toBe(chip);
  });
});
