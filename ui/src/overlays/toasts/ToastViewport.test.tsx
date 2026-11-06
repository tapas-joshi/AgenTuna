import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ToastProvider, useToast } from "./ToastProvider";
import { ToastViewport } from "./ToastViewport";
import { createToastStore } from "./store";
import { createFakeClock } from "./testing";
import type { ClipboardPort } from "./types";

afterEach(cleanup);

function clipboardStub(): ClipboardPort {
  return { writeText: vi.fn(async () => undefined) };
}

function renderToastViewport(clipboard: ClipboardPort = clipboardStub()) {
  const clock = createFakeClock();
  const store = createToastStore({ clock });
  render(<ToastViewport store={store} clipboard={clipboard} />);
  return { clock, store, clipboard };
}

describe("toast viewport", () => {
  test("announces non-error toasts in the polite live region", () => {
    const { store } = renderToastViewport();
    act(() => {
      store.show({ kind: "success", message: "Skill enabled" });
    });

    const polite = screen.getByTestId("toast-live-polite");
    expect(polite.getAttribute("role")).toBe("status");
    expect(polite.getAttribute("aria-live")).toBe("polite");
    expect(polite.textContent).toContain("Success: Skill enabled");
    expect(screen.getByTestId("toast-live-assertive").textContent).toBe("");
    expect(screen.getByText("Skill enabled")).toBeTruthy();
  });

  test("announces errors in the assertive live region without moving focus", () => {
    const outside = document.createElement("button");
    outside.textContent = "outside";
    document.body.append(outside);
    outside.focus();

    try {
      const { store } = renderToastViewport();
      act(() => {
        store.show({ kind: "error", message: "Cannot write the file" });
      });

      const assertive = screen.getByTestId("toast-live-assertive");
      expect(assertive.getAttribute("role")).toBe("alert");
      expect(assertive.getAttribute("aria-live")).toBe("assertive");
      expect(assertive.textContent).toContain("Error: Cannot write the file");
      expect(document.activeElement).toBe(outside);
    } finally {
      outside.remove();
    }
  });

  test("error toasts expose Copy details and report the copy result", async () => {
    const user = userEvent.setup();
    const clipboard = clipboardStub();
    const { store } = renderToastViewport(clipboard);

    act(() => {
      store.show({
        kind: "error",
        message: "Cannot write /tmp/x",
        title: "E_PERMISSION_DENIED",
        details: "E_PERMISSION_DENIED\nCannot write /tmp/x",
      });
    });

    await user.click(screen.getByRole("button", { name: "Copy details" }));

    expect(clipboard.writeText).toHaveBeenCalledTimes(1);
    const copied = vi.mocked(clipboard.writeText).mock.calls[0]?.[0] ?? "";
    expect(copied).toContain("[Error]");
    expect(copied).toContain("E_PERMISSION_DENIED");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeTruthy();
  });

  test("every error toast exposes Copy details, even without a details payload", () => {
    const { store } = renderToastViewport();
    act(() => {
      store.show({ kind: "error", message: "Something broke" });
    });
    expect(screen.getByRole("button", { name: "Copy details" })).toBeTruthy();
  });

  test("a failed copy is reported instead of pretending success", async () => {
    const user = userEvent.setup();
    const clipboard: ClipboardPort = {
      writeText: vi.fn(() => Promise.reject(new Error("no clipboard"))),
    };
    const { store } = renderToastViewport(clipboard);

    act(() => {
      store.show({ kind: "error", message: "boom", details: "boom" });
    });
    await user.click(screen.getByRole("button", { name: "Copy details" }));

    expect(await screen.findByRole("button", { name: "Copy failed" })).toBeTruthy();
  });

  test("shows a count badge when identical toasts collapse", () => {
    const { store } = renderToastViewport();
    act(() => {
      store.show({ kind: "warning", message: "Disk full" });
      store.show({ kind: "warning", message: "Disk full" });
    });

    expect(screen.getAllByText("Disk full")).toHaveLength(1);
    expect(screen.getByTestId("toast-live-polite").textContent).toContain("(2 occurrences)");
    expect(screen.getByText("×2")).toBeTruthy();
    expect(screen.getByLabelText("2 occurrences")).toBeTruthy();
  });

  test("closing a toast removes it and reveals the next queued toast", async () => {
    const user = userEvent.setup();
    const { store } = renderToastViewport();
    act(() => {
      for (const message of ["one", "two", "three", "four"]) {
        store.show({ kind: "info", message });
      }
    });
    expect(screen.queryByText("four")).toBeNull();

    await user.click(screen.getByRole("button", { name: "Dismiss notification: one" }));
    expect(screen.queryByText("one")).toBeNull();
    expect(screen.getByText("four")).toBeTruthy();
  });

  test("the action button runs the action and dismisses the toast", async () => {
    const user = userEvent.setup();
    const run = vi.fn();
    const { store } = renderToastViewport();
    act(() => {
      store.show({ kind: "success", message: "Moved", action: { label: "Undo", run } });
    });

    await user.click(screen.getByRole("button", { name: "Undo" }));
    expect(run).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("Moved")).toBeNull();
  });

  test("kind is spelled out so colour never carries the meaning alone", () => {
    const { store } = renderToastViewport();
    act(() => {
      store.show({ kind: "warning", message: "Storage nearly full" });
    });
    expect(screen.getByText("Warning")).toBeTruthy();
    expect(screen.getByText("Storage nearly full")).toBeTruthy();
  });
});

function Notify() {
  const toast = useToast();
  return (
    <button
      type="button"
      onClick={() => {
        toast.success("Provider wired");
      }}
    >
      notify
    </button>
  );
}

describe("toast provider", () => {
  test("renders the viewport once and exposes useToast to the tree", async () => {
    const user = userEvent.setup();
    const store = createToastStore({ clock: createFakeClock() });
    render(
      <ToastProvider store={store} clipboard={clipboardStub()}>
        <Notify />
      </ToastProvider>,
    );

    expect(screen.queryByText("Provider wired")).toBeNull();
    await user.click(screen.getByRole("button", { name: "notify" }));
    expect(screen.getByText("Provider wired")).toBeTruthy();

    store.dismissAll();
  });

  test("useToast outside a provider fails loudly", () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      expect(() => render(<Notify />)).toThrow(/ToastProvider/);
    } finally {
      errorSpy.mockRestore();
    }
  });
});
