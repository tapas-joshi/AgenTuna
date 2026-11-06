import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useRef, useState } from "react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { OVERLAY_Z_MAX } from "../stack/OverlayStack";
import type { AnchorRect } from "./anchor";
import { isAnchorRect, virtualAnchorFromRect } from "./anchor";
import { POPOVER_VIEWPORT_GUTTER, Popover } from "./Popover";

beforeEach(() => {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: 1024 });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: 768 });
  // floating-ui reads the layout viewport from the html element in jsdom.
  Object.defineProperty(document.documentElement, "clientWidth", { configurable: true, value: 1024 });
  Object.defineProperty(document.documentElement, "clientHeight", { configurable: true, value: 768 });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/** A DOMRect-shaped rectangle for stubs and captured anchors. */
function rect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    x: left,
    y: top,
    toJSON: () => ({}),
  } as DOMRect;
}

/** Makes the floating wrapper report a fixed size, as a real panel would. */
function stubFloatingSize(width: number, height: number): void {
  const isPanel = (element: Element): boolean =>
    element.hasAttribute("data-radix-popper-content-wrapper");

  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (
    this: Element,
  ): DOMRect {
    return isPanel(this) ? rect(0, 0, width, height) : rect(0, 0, 0, 0);
  });
  // floating-ui sizes the panel from offsetWidth/offsetHeight, which jsdom
  // leaves at zero.
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ): number {
    return isPanel(this) ? width : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ): number {
    return isPanel(this) ? height : 0;
  });
}

/** Reads the translate(x, y) floating-ui writes on the panel wrapper. */
function panelOffset(): { readonly x: number; readonly y: number } | null {
  const wrapper = document.querySelector<HTMLElement>("[data-radix-popper-content-wrapper]");
  const match = wrapper?.style.transform.match(/translate\((-?[\d.]+)px,\s*(-?[\d.]+)px\)/);
  if (match === null || match === undefined) {
    return null;
  }
  return { x: Number(match[1]), y: Number(match[2]) };
}

interface HarnessProps {
  readonly anchor?: Element | AnchorRect | null;
  readonly onOpenChange?: (open: boolean) => void;
  readonly withInput?: boolean;
}

/** A trigger button that owns the open state and gives the panel a real anchor. */
function Harness({ anchor, onOpenChange, withInput = true }: HarnessProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  return (
    <>
      <button ref={triggerRef} type="button" onClick={() => setOpen(true)}>
        Open project picker
      </button>
      <button type="button" onClick={() => setOpen(false)}>
        Somewhere else
      </button>
      <Popover
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          onOpenChange?.(next);
        }}
        anchor={anchor ?? triggerRef.current}
        ariaLabel="Choose a project"
      >
        {withInput ? <input aria-label="Filter projects" /> : <p>No projects yet</p>}
      </Popover>
    </>
  );
}

describe("anchor helpers", () => {
  test("a live element is not mistaken for a rectangle", () => {
    const element = document.createElement("button");
    expect(isAnchorRect(element)).toBe(false);
    expect(isAnchorRect({ top: 1, left: 2, width: 3, height: 4 })).toBe(true);
  });

  test("a captured rectangle gains the DOMRect fields floating-ui reads", () => {
    const anchor = virtualAnchorFromRect({ top: 10, left: 20, width: 30, height: 40 });

    expect(anchor.getBoundingClientRect()).toMatchObject({
      top: 10,
      left: 20,
      width: 30,
      height: 40,
      right: 50,
      bottom: 50,
    });
    expect(anchor.getClientRects()).toHaveLength(1);
  });
});

describe("Popover", () => {
  async function openHarness(props: Partial<HarnessProps> = {}) {
    const user = userEvent.setup();
    const view = render(<Harness anchor={null} {...props} />);
    await user.click(screen.getByRole("button", { name: "Open project picker" }));
    return { user, view };
  }

  test("renders a named dialog panel in a portal, not inside the trigger's container", async () => {
    stubFloatingSize(320, 200);
    await openHarness();

    const panel = screen.getByRole("dialog", { name: "Choose a project" });
    expect(panel.classList.contains("at-popover")).toBe(true);
    // Portalled out of the trigger's React tree and into the document body.
    expect(panel.parentElement?.closest("[data-testid]")).toBeNull();
    expect(Number(panel.style.zIndex)).toBeLessThanOrEqual(OVERLAY_Z_MAX);
  });

  test("anchors below a captured widget rect, offset by the gutter", async () => {
    stubFloatingSize(240, 120);
    const anchor = { top: 100, left: 50, width: 20, height: 30 };
    await openHarness({ anchor });

    await waitFor(() => expect(panelOffset()).toEqual({ x: 50, y: 100 + 30 + POPOVER_VIEWPORT_GUTTER }));
  });

  test("repositions when the anchor scrolls", async () => {
    stubFloatingSize(240, 120);
    const anchor = { top: 100, left: 50, width: 20, height: 30 };
    await openHarness({ anchor });
    await waitFor(() => expect(panelOffset()).not.toBeNull());

    anchor.top = 260;
    window.dispatchEvent(new Event("scroll"));

    await waitFor(() => expect(panelOffset()).toEqual({ x: 50, y: 260 + 30 + POPOVER_VIEWPORT_GUTTER }));
  });

  test("repositions when the window resizes", async () => {
    stubFloatingSize(240, 120);
    const anchor = { top: 100, left: 50, width: 20, height: 30 };
    await openHarness({ anchor });
    await waitFor(() => expect(panelOffset()).not.toBeNull());

    anchor.top = 320;
    window.dispatchEvent(new Event("resize"));

    await waitFor(() => expect(panelOffset()?.y).toBe(320 + 30 + POPOVER_VIEWPORT_GUTTER));
  });

  test("never leaves the panel outside the viewport", async () => {
    const width = 300;
    const height = 200;
    stubFloatingSize(width, height);
    // Anchor in the bottom-right corner: neither below nor to the right there is
    // room, so the panel has to flip and shift.
    await openHarness({ anchor: { top: 740, left: 1000, width: 20, height: 20 } });

    await waitFor(() => {
      const offset = panelOffset();
      expect(offset).not.toBeNull();
      expect(offset?.x ?? -1).toBeGreaterThanOrEqual(POPOVER_VIEWPORT_GUTTER);
      expect(offset?.y ?? -1).toBeGreaterThanOrEqual(POPOVER_VIEWPORT_GUTTER);
      expect((offset?.x ?? 0) + width).toBeLessThanOrEqual(1024 - POPOVER_VIEWPORT_GUTTER);
      expect((offset?.y ?? 0) + height).toBeLessThanOrEqual(768 - POPOVER_VIEWPORT_GUTTER);
    });
  });

  test("moves focus inside on open and returns it to the opener on Escape", async () => {
    stubFloatingSize(320, 200);
    const onOpenChange = vi.fn();
    const { user } = await openHarness({ onOpenChange });

    expect(document.activeElement).toBe(screen.getByLabelText("Filter projects"));

    await user.keyboard("{Escape}");

    expect(onOpenChange).toHaveBeenCalledWith(false);
    await waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole("button", { name: "Open project picker" }),
      ),
    );
  });

  test("an outside click dismisses the panel", async () => {
    stubFloatingSize(320, 200);
    const onOpenChange = vi.fn();
    const { user } = await openHarness({ onOpenChange });

    await user.click(screen.getByRole("button", { name: "Somewhere else" }));

    expect(onOpenChange).toHaveBeenCalledWith(false);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  test("a click inside the panel does not dismiss it", async () => {
    stubFloatingSize(320, 200);
    const onOpenChange = vi.fn();
    await openHarness({ onOpenChange, withInput: false });

    const panel = screen.getByRole("dialog", { name: "Choose a project" });
    await userEvent.setup().click(panel);

    expect(onOpenChange).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Choose a project" })).toBeTruthy();
  });

  test("stays closed when open is false", () => {
    render(<Harness anchor={null} />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  test("stays closed while the anchor is unknown", () => {
    render(<Popover open anchor={null} ariaLabel="Unanchored">
      <p>No position yet</p>
    </Popover>);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
