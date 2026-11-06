import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test } from "vitest";
import { Modal } from "./dialogs";
import { Menu, MenuItem } from "./menus";
import { Popover } from "./popovers";
import { OVERLAY_Z_MAX, TOAST_VIEWPORT_Z } from "./stack";

afterEach(cleanup);

/** A modal that opens a menu and a popover from controls inside its panel. */
function NestedOverlays() {
  const [menuOpen, setMenuOpen] = useState(false);
  const [popoverOpen, setPopoverOpen] = useState(false);

  return (
    <Modal open title="Skill actions">
      <button type="button" onClick={() => setMenuOpen(true)}>
        Open menu
      </button>
      <button type="button" onClick={() => setPopoverOpen(true)}>
        Open popover
      </button>
      <Menu
        open={menuOpen}
        onOpenChange={setMenuOpen}
        modal={false}
        trigger={<button type="button">Actions</button>}
      >
        <MenuItem label="Open" onSelect={() => undefined} />
      </Menu>
      <Popover
        open={popoverOpen}
        onOpenChange={setPopoverOpen}
        anchor={{ top: 200, left: 200, width: 20, height: 20 }}
        ariaLabel="Details"
      >
        <p>Details</p>
      </Popover>
    </Modal>
  );
}

function modalLayer(): number {
  const panel = document.querySelector<HTMLElement>(".at-modal");
  return Number(panel?.style.zIndex);
}

describe("overlay stacking", () => {
  test("a menu opened from inside a modal paints above that modal", async () => {
    const user = userEvent.setup();
    render(<NestedOverlays />);

    await user.click(screen.getByRole("button", { name: "Open menu" }));

    const menu = screen.getByRole("menu");
    expect(Number(menu.style.zIndex)).toBeGreaterThan(modalLayer());
    expect(Number(menu.style.zIndex)).toBeLessThan(TOAST_VIEWPORT_Z);
  });

  test("a popover opened from inside a modal paints above that modal", async () => {
    const user = userEvent.setup();
    render(<NestedOverlays />);

    await user.click(screen.getByRole("button", { name: "Open popover" }));

    const panel = screen.getByRole("dialog", { name: "Details" });
    expect(Number(panel.style.zIndex)).toBeGreaterThan(modalLayer());
    expect(Number(panel.style.zIndex)).toBeLessThanOrEqual(OVERLAY_Z_MAX);
  });

  test("closing the nested surface releases its layer above the modal", async () => {
    const user = userEvent.setup();
    render(<NestedOverlays />);

    await user.click(screen.getByRole("button", { name: "Open menu" }));
    expect(screen.getByRole("menu")).toBeTruthy();

    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

    await user.click(screen.getByRole("button", { name: "Open popover" }));
    const panel = screen.getByRole("dialog", { name: "Details" });
    expect(Number(panel.style.zIndex)).toBeGreaterThan(modalLayer());
  });
});

/** A popover that owns a menu, so Escape order can cross surface kinds. */
function PopoverWithMenu() {
  const [popoverOpen, setPopoverOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <Modal open title="Project details">
      <button type="button" onClick={() => setPopoverOpen(true)}>
        Open project popover
      </button>
      <Popover
        open={popoverOpen}
        onOpenChange={setPopoverOpen}
        anchor={{ top: 200, left: 200, width: 20, height: 20 }}
        ariaLabel="Project"
      >
        <Menu
          open={menuOpen}
          onOpenChange={setMenuOpen}
          modal={false}
          trigger={<button type="button">Project actions</button>}
        >
          <MenuItem label="Open" onSelect={() => undefined} />
        </Menu>
      </Popover>
    </Modal>
  );
}

describe("Escape order across kinds", () => {
  test("Escape closes the menu, then the popover, and leaves the modal open", async () => {
    const user = userEvent.setup();
    render(<PopoverWithMenu />);

    await user.click(screen.getByRole("button", { name: "Open project popover" }));
    await user.click(screen.getByRole("button", { name: "Project actions" }));
    expect(screen.getByRole("menu")).toBeTruthy();

    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(screen.getByRole("dialog", { name: "Project" })).toBeTruthy();

    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Project" })).toBeNull());
    expect(screen.getByRole("dialog", { name: "Project details" })).toBeTruthy();
  });
});
