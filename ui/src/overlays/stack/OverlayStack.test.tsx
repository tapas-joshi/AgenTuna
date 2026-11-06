import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
  OVERLAY_Z_BASE,
  OVERLAY_Z_MAX,
  TOAST_VIEWPORT_Z,
  createOverlayStack,
} from "./OverlayStack";
import { OverlayStackProvider, useOverlayLayer } from "./OverlayStackProvider";

afterEach(cleanup);

const sourceRoot = join(__dirname, "..", "..");

describe("createOverlayStack", () => {
  test("allocates increasing layers from the base of the band", () => {
    const stack = createOverlayStack({ createId: () => `layer-${stackPushCount++}` });
    const first = stack.push("modal");
    const second = stack.push("popover");

    expect(first.depth).toBe(0);
    expect(first.zIndex).toBe(OVERLAY_Z_BASE);
    expect(second.depth).toBe(1);
    expect(second.zIndex).toBe(OVERLAY_Z_BASE + 1);
    expect(stack.top()).toBe(second);
    expect(stack.layers()).toEqual([first, second]);
  });

  test("closing a middle layer never reorders the layers still open", () => {
    const stack = createOverlayStack();
    const modal = stack.push("modal");
    const popover = stack.push("popover");
    const menu = stack.push("menu");

    stack.remove(popover.id);

    expect(stack.layers()).toEqual([modal, menu]);
    expect(menu.zIndex).toBeGreaterThan(modal.zIndex);
    expect(stack.top()).toBe(menu);
  });

  test("removing an unknown or already removed id is a no-op", () => {
    const stack = createOverlayStack();
    const layer = stack.push("modal");
    const notify = vi.fn();
    stack.subscribe(notify);

    stack.remove("missing");
    stack.remove(layer.id);
    stack.remove(layer.id);

    expect(stack.layers()).toEqual([]);
    expect(stack.top()).toBeNull();
    expect(notify).toHaveBeenCalledTimes(1);
  });

  test("depth stays below the toast viewport however many layers open", () => {
    const stack = createOverlayStack();
    const layers = Array.from({ length: 40 }, () => stack.push("modal"));

    const top = layers[layers.length - 1];
    expect(top?.zIndex).toBe(OVERLAY_Z_MAX);
    expect(OVERLAY_Z_MAX).toBeLessThan(TOAST_VIEWPORT_Z);
    expect(OVERLAY_Z_BASE).toBeLessThan(OVERLAY_Z_MAX);
  });

  test("subscribers hear every change and stop when unsubscribed", () => {
    const stack = createOverlayStack();
    const notify = vi.fn();
    const unsubscribe = stack.subscribe(notify);

    const layer = stack.push("menu");
    stack.remove(layer.id);
    unsubscribe();
    stack.push("menu");

    expect(notify).toHaveBeenCalledTimes(2);
  });
});

describe("toast stacking contract", () => {
  test("the toast viewport z-index is the reserved value the stack caps against", () => {
    const css = readFileSync(join(sourceRoot, "overlays", "toasts", "toasts.css"), "utf8");
    const match = /z-index:\s*(\d+)\s*;/.exec(css);

    expect(match, "toasts.css declares a viewport z-index").not.toBeNull();
    expect(Number(match?.[1])).toBe(TOAST_VIEWPORT_Z);
  });
});

let stackPushCount = 0;

/** Renders one surface per kind so the hook's allocation order is observable. */
function StackHarness() {
  const [open, setOpen] = useState({ modal: false, popover: false, menu: false });
  const modal = useOverlayLayer("modal", open.modal);
  const popover = useOverlayLayer("popover", open.popover);
  const menu = useOverlayLayer("menu", open.menu);

  return (
    <div>
      <button onClick={() => setOpen({ modal: true, popover: false, menu: false })}>Open modal</button>
      <button
        onClick={() => setOpen({ modal: true, popover: true, menu: false })}
        disabled={!open.modal}
      >
        Open popover
      </button>
      <button
        onClick={() => setOpen({ modal: true, popover: true, menu: true })}
        disabled={!open.popover}
      >
        Open menu
      </button>
      <button onClick={() => setOpen({ modal: false, popover: false, menu: false })}>Close all</button>
      <div data-testid="modal-layer" style={{ zIndex: modal }} />
      <div data-testid="popover-layer" style={{ zIndex: popover }} />
      <div data-testid="menu-layer" style={{ zIndex: menu }} />
    </div>
  );
}

describe("useOverlayLayer", () => {
  test("a nested popover takes a higher layer than the modal that owns it", async () => {
    const user = userEvent.setup();
    const stack = createOverlayStack();
    render(
      <OverlayStackProvider stack={stack}>
        <StackHarness />
      </OverlayStackProvider>,
    );

    await user.click(screen.getByRole("button", { name: "Open modal" }));
    const modalZ = Number(screen.getByTestId("modal-layer").style.zIndex);

    await user.click(screen.getByRole("button", { name: "Open popover" }));
    const popoverZ = Number(screen.getByTestId("popover-layer").style.zIndex);

    expect(popoverZ).toBeGreaterThan(modalZ);
    expect(stack.layers().map((layer) => layer.kind)).toEqual(["modal", "popover"]);
  });

  test("closing releases the layer so the next surface reuses the band", async () => {
    const user = userEvent.setup();
    const stack = createOverlayStack();
    render(
      <OverlayStackProvider stack={stack}>
        <StackHarness />
      </OverlayStackProvider>,
    );

    await user.click(screen.getByRole("button", { name: "Open modal" }));
    expect(stack.layers()).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Close all" }));
    expect(stack.layers()).toHaveLength(0);

    await user.click(screen.getByRole("button", { name: "Open modal" }));
    expect(Number(screen.getByTestId("modal-layer").style.zIndex)).toBe(OVERLAY_Z_BASE);
  });

  test("stack changes published inside an act flush to the next render", () => {
    const stack = createOverlayStack();
    act(() => {
      stack.push("modal");
    });
    expect(stack.top()?.kind).toBe("modal");
  });
});
