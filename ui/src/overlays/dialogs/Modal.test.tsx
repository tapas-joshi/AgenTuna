import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { UserEvent } from "@testing-library/user-event";
import { useRef, useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { OVERLAY_Z_MAX, TOAST_VIEWPORT_Z } from "../stack/OverlayStack";
import { Modal } from "./Modal";

afterEach(cleanup);

interface HarnessProps {
  readonly dismissible?: boolean;
  readonly initialFocus?: "cancel" | "none";
  readonly onOpenChange?: (open: boolean) => void;
}

/** Opens a modal from a real button so focus restoration has an opener. */
function ModalHarness({ dismissible = true, initialFocus = "none", onOpenChange }: HarnessProps) {
  const [open, setOpen] = useState(false);
  const cancelRef = useRef<HTMLButtonElement>(null);

  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open adopt
      </button>
      <Modal
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          onOpenChange?.(next);
        }}
        title="Adopt release-notes"
        description="Review the original and the destination first."
        dismissible={dismissible}
        initialFocusRef={initialFocus === "cancel" ? cancelRef : undefined}
        footer={
          <>
            <button ref={cancelRef} type="button">
              Cancel
            </button>
            <button type="button">Adopt</button>
          </>
        }
      >
        <input aria-label="Backup label" />
        <button type="button">Open details</button>
      </Modal>
    </>
  );
}

/** Two independently controlled dialogs, one over the other. */
function StackedHarness() {
  const [bottom, setBottom] = useState(true);
  const [top, setTop] = useState(true);

  return (
    <>
      <Modal open={bottom} onOpenChange={setBottom} title="Bottom">
        <button type="button">Bottom action</button>
      </Modal>
      <Modal open={top} onOpenChange={setTop} title="Top">
        <button type="button">Top action</button>
      </Modal>
    </>
  );
}

async function openAdopt(): Promise<{ user: UserEvent; opener: HTMLElement }> {
  const user = userEvent.setup();
  render(<ModalHarness />);
  const opener = screen.getByRole("button", { name: "Open adopt" });
  await user.click(opener);
  return { user, opener };
}

describe("Modal", () => {
  test("renders a named modal dialog that describes itself", async () => {
    await openAdopt();

    const dialog = screen.getByRole("dialog", { name: "Adopt release-notes" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("data-size")).toBe("md");

    const describedBy = dialog.getAttribute("aria-describedby");
    expect(describedBy).not.toBeNull();
    expect(document.getElementById(describedBy ?? "")?.textContent).toBe(
      "Review the original and the destination first.",
    );
  });

  test("renders nothing while closed", () => {
    render(<ModalHarness />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  test("moves focus inside on open and back to the opener on Escape", async () => {
    const onOpenChange = vi.fn();
    const user = userEvent.setup();
    render(<ModalHarness onOpenChange={onOpenChange} />);
    const opener = screen.getByRole("button", { name: "Open adopt" });

    await user.click(opener);
    const dialog = screen.getByRole("dialog", { name: "Adopt release-notes" });
    expect(dialog.contains(document.activeElement)).toBe(true);

    await user.keyboard("{Escape}");

    expect(onOpenChange).toHaveBeenCalledWith(false);
    await waitFor(() => expect(document.activeElement).toBe(opener));
  });

  test("honours initialFocusRef instead of the first tabbable control", async () => {
    const user = userEvent.setup();
    render(<ModalHarness initialFocus="cancel" />);

    await user.click(screen.getByRole("button", { name: "Open adopt" }));

    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
  });

  test("Tab cycles inside the panel instead of escaping it", async () => {
    const { user } = await openAdopt();
    const first = screen.getByLabelText("Backup label");
    const last = screen.getByRole("button", { name: "Adopt" });

    expect(document.activeElement).toBe(first);

    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Open details" }));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" }));
    await user.tab();
    expect(document.activeElement).toBe(last);
    await user.tab();
    expect(document.activeElement).toBe(first);
  });

  test("locks page scroll while open and releases it on close", async () => {
    const { user } = await openAdopt();
    // Radix RemoveScroll locks the body with an attribute and an injected rule;
    // the attribute is the observable contract that the page cannot scroll.
    expect(document.body.hasAttribute("data-scroll-locked")).toBe(true);

    await user.keyboard("{Escape}");

    await waitFor(() => expect(document.body.hasAttribute("data-scroll-locked")).toBe(false));
  });

  test("a backdrop click dismisses a normal modal", async () => {
    const onOpenChange = vi.fn();
    const user = userEvent.setup();
    render(<ModalHarness onOpenChange={onOpenChange} />);
    await user.click(screen.getByRole("button", { name: "Open adopt" }));

    const backdrop = document.querySelector(".at-modal__backdrop");
    expect(backdrop).not.toBeNull();
    await user.click(backdrop as Element);

    expect(onOpenChange).toHaveBeenCalledWith(false);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  test("a destructive modal ignores Escape and outside clicks", async () => {
    const onOpenChange = vi.fn();
    const user = userEvent.setup();
    render(<ModalHarness dismissible={false} onOpenChange={onOpenChange} />);
    await user.click(screen.getByRole("button", { name: "Open adopt" }));

    await user.keyboard("{Escape}");
    await user.click(document.querySelector(".at-modal__backdrop") as Element);

    expect(onOpenChange).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Adopt release-notes" })).toBeTruthy();
  });

  test("a second modal takes a higher layer and Escape closes only the top one", async () => {
    const user = userEvent.setup();
    render(<StackedHarness />);

    const dialogs = document.querySelectorAll<HTMLElement>(".at-modal");
    expect(dialogs).toHaveLength(2);
    const bottom = dialogs[0] as HTMLElement;
    const top = dialogs[1] as HTMLElement;
    expect(Number(top.style.zIndex)).toBeGreaterThan(Number(bottom.style.zIndex));

    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Top" })).toBeNull());
    expect(screen.getByRole("dialog", { name: "Bottom" })).toBeTruthy();

    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Bottom" })).toBeNull());
  });

  test("the page behind a modal is hidden from assistive technology", async () => {
    const user = userEvent.setup();
    render(<ModalHarness />);
    const page = screen.getByRole("button", { name: "Open adopt" }).parentElement;
    expect(page?.getAttribute("aria-hidden")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Open adopt" }));
    expect(page?.getAttribute("aria-hidden")).toBe("true");

    await user.keyboard("{Escape}");
    await waitFor(() => expect(page?.getAttribute("aria-hidden")).toBeNull());
  });

  test("overlay layers never reach the toast viewport", async () => {
    await openAdopt();
    const dialog = screen.getByRole("dialog", { name: "Adopt release-notes" });

    expect(Number(dialog.style.zIndex)).toBeLessThanOrEqual(OVERLAY_Z_MAX);
    expect(TOAST_VIEWPORT_Z).toBeGreaterThan(OVERLAY_Z_MAX);
  });
});
