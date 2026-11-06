import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OVERLAY_Z_MAX } from "../stack/OverlayStack";
import { Menu, MenuLabel, MenuSeparator } from "./Menu";
import { MenuItem } from "./MenuItem";

afterEach(cleanup);

interface HarnessProps {
  readonly onSelect?: (item: string) => void;
  readonly onOpenChange?: (open: boolean) => void;
}

/** A controlled menu with one disabled row between enabled ones. */
function MenuHarness({ onSelect, onOpenChange }: HarnessProps) {
  const [open, setOpen] = useState(false);
  const [chosen, setChosen] = useState("none");

  const choose = (item: string): void => {
    setChosen(item);
    onSelect?.(item);
  };

  return (
    <>
      <Menu
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          onOpenChange?.(next);
        }}
        trigger={<button type="button">Actions</button>}
      >
        <MenuLabel>Actions</MenuLabel>
        <MenuItem label="Open" shortcut="⌘O" onSelect={() => choose("open")} />
        <MenuItem
          label="Adopt"
          disabled
          disabledReason="Not in Studio yet"
          onSelect={() => choose("adopt")}
        />
        <MenuSeparator />
        <MenuItem label="Delete" tone="danger" mnemonic="D" onSelect={() => choose("delete")} />
        <MenuItem label="Archive" mnemonic="A" onSelect={() => choose("archive")} />
      </Menu>
      <output data-testid="chosen">{chosen}</output>
    </>
  );
}

const trigger = (): HTMLElement => screen.getByRole("button", { name: "Actions" });

describe("Menu", () => {
  test("the trigger advertises a menu and its expanded state", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);

    const button = trigger();
    expect(button.getAttribute("aria-haspopup")).toBe("menu");
    expect(button.getAttribute("aria-expanded")).toBe("false");

    await user.click(button);

    // Radix hides the page behind a modal menu, so keep the element reference.
    expect(button.getAttribute("aria-expanded")).toBe("true");
  });

  test("renders a named menu with menuitems, a label and a separator", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);
    await user.click(trigger());

    const menu = screen.getByRole("menu", { name: "Actions" });
    expect(menu).toBeTruthy();
    expect(screen.getAllByRole("menuitem")).toHaveLength(4);
    expect(screen.getByRole("separator")).toBeTruthy();
    expect(screen.getByText("Actions", { selector: ".at-menu__heading" })).toBeTruthy();
  });

  test("ArrowDown walks enabled items only and Up walks back", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);
    await user.click(trigger());

    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: /Open/ }));

    // The disabled Adopt row is skipped by keyboard navigation.
    await user.keyboard("{ArrowDown}");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Delete" }));

    await user.keyboard("{ArrowUp}");
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: /Open/ }));
  });

  test("Enter selects the focused item and closes the menu", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<MenuHarness onSelect={onSelect} />);
    await user.click(trigger());

    await user.keyboard("{ArrowDown}{Enter}");

    expect(onSelect).toHaveBeenCalledWith("open");
    expect(screen.getByTestId("chosen").textContent).toBe("open");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  test("a disabled row cannot be selected and states why", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<MenuHarness onSelect={onSelect} />);
    await user.click(trigger());

    const adopt = screen.getByRole("menuitem", { name: /Adopt/ });
    expect(adopt.getAttribute("aria-disabled")).toBe("true");

    await user.click(adopt);

    expect(onSelect).not.toHaveBeenCalled();
    expect(screen.getByRole("menu")).toBeTruthy();
    // Screen readers get the reason in the row; hovering shows the tooltip.
    expect(screen.getByText("Unavailable: Not in Studio yet")).toBeTruthy();
  });

  test("hovering a disabled row explains it in a tooltip", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);
    await user.click(trigger());

    await user.hover(screen.getByRole("menuitem", { name: /Adopt/ }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip.textContent).toBe("Not in Studio yet");
  });

  test("an Alt+mnemonic selects its item and closes the menu", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<MenuHarness onSelect={onSelect} />);
    await user.click(trigger());

    const archive = screen.getByRole("menuitem", { name: "Archive" });
    expect(archive.getAttribute("aria-keyshortcuts")).toBe("Alt+A");
    expect(archive.querySelector("u")?.textContent).toBe("A");

    await user.keyboard("{Alt>}a{/Alt}");

    expect(onSelect).toHaveBeenCalledWith("archive");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  test("Escape closes the menu", async () => {
    const user = userEvent.setup();
    const onOpenChange = vi.fn();
    render(<MenuHarness onOpenChange={onOpenChange} />);
    await user.click(trigger());

    await user.keyboard("{Escape}");

    expect(onOpenChange).toHaveBeenCalledWith(false);
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  });

  test("the menu layer stays under the toast viewport", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);
    await user.click(trigger());

    const menu = screen.getByRole("menu");
    expect(Number(menu.style.zIndex)).toBeLessThanOrEqual(OVERLAY_Z_MAX);
  });
});
