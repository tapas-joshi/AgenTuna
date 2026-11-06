import { describe, expect, test, vi } from "vitest";
import {
  AUTO_DISMISS_MS,
  COLLAPSE_WITHIN_MS,
  MAX_VISIBLE_TOASTS,
  createToastStore,
} from "./store";
import type { ToastStore } from "./store";
import { createFakeClock, type FakeClock } from "./testing";
import type { Toast, ToastInput } from "./types";

interface Harness {
  readonly clock: FakeClock;
  readonly store: ToastStore;
}

function setup(): Harness {
  const clock = createFakeClock();
  return { clock, store: createToastStore({ clock }) };
}

function visibleIds(store: ToastStore): string[] {
  return store.getSnapshot().visible.map((toast: Toast) => toast.id);
}

function info(message: string): ToastInput {
  return { kind: "info", message };
}

describe("toast store visibility and expiry", () => {
  test("keeps at most three toasts visible and promotes in FIFO order", () => {
    const { store } = setup();
    const ids = ["one", "two", "three", "four", "five"].map((message) =>
      store.show(info(message)),
    );

    expect(MAX_VISIBLE_TOASTS).toBe(3);
    expect(visibleIds(store)).toEqual(ids.slice(0, 3));
    expect(store.getSnapshot().queued).toBe(2);

    store.dismiss(ids[0] ?? "");
    expect(visibleIds(store)).toEqual([ids[1], ids[2], ids[3]]);
    expect(store.getSnapshot().queued).toBe(1);
  });

  test.each<ToastInput["kind"]>(["success", "warning", "info"])(
    "auto-dismisses a %s toast after exactly 2.5 s on the injected clock",
    (kind) => {
      const { clock, store } = setup();
      store.show({ kind, message: "saved" });

      clock.advance(AUTO_DISMISS_MS - 1);
      expect(store.getSnapshot().visible).toHaveLength(1);

      clock.advance(1);
      expect(store.getSnapshot().visible).toHaveLength(0);
    },
  );

  test("the default clock schedules through the host timers", () => {
    vi.useFakeTimers();
    try {
      const store = createToastStore();
      store.show({ kind: "success", message: "timed" });

      vi.advanceTimersByTime(AUTO_DISMISS_MS - 1);
      expect(store.getSnapshot().visible).toHaveLength(1);
      vi.advanceTimersByTime(1);
      expect(store.getSnapshot().visible).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });

  test("error toasts never auto-dismiss", () => {
    const { clock, store } = setup();
    const id = store.show({ kind: "error", message: "write failed" });

    clock.advance(AUTO_DISMISS_MS * 40);
    expect(store.getSnapshot().visible.map((toast) => toast.id)).toEqual([id]);

    store.dismiss(id);
    expect(store.getSnapshot().visible).toHaveLength(0);
  });

  test("a queued toast starts its timer when it becomes visible", () => {
    const { clock, store } = setup();
    const first = store.show(info("one"));
    store.show(info("two"));
    store.show(info("three"));
    const fourth = store.show(info("four"));
    expect(store.getSnapshot().queued).toBe(1);

    clock.advance(AUTO_DISMISS_MS);
    expect(visibleIds(store)).toEqual([fourth]);

    clock.advance(AUTO_DISMISS_MS - 1);
    expect(store.getSnapshot().visible).toHaveLength(1);

    clock.advance(1);
    expect(store.getSnapshot().visible).toHaveLength(0);
    expect(first).not.toBe(fourth);
  });

  test("dismissing a queued toast leaves the visible stack untouched", () => {
    const { store } = setup();
    const ids = [0, 1, 2, 3].map((index) => store.show(info(`m${index}`)));

    store.dismiss(ids[3] ?? "");
    expect(visibleIds(store)).toEqual(ids.slice(0, 3));
    expect(store.getSnapshot().queued).toBe(0);
  });
});

describe("toast collapse", () => {
  test("an identical toast within one second increments the existing card", () => {
    const { clock, store } = setup();
    store.show({ kind: "warning", message: "disk full" });
    clock.advance(COLLAPSE_WITHIN_MS);
    const id = store.show({ kind: "warning", message: "disk full" });

    const visible = store.getSnapshot().visible;
    expect(visible).toHaveLength(1);
    expect(visible[0]?.id).toBe(id);
    expect(visible[0]?.count).toBe(2);
  });

  test("an identical toast after the window becomes a separate card", () => {
    const { clock, store } = setup();
    store.show({ kind: "warning", message: "disk full" });
    clock.advance(COLLAPSE_WITHIN_MS + 1);
    store.show({ kind: "warning", message: "disk full" });

    expect(store.getSnapshot().visible).toHaveLength(2);
  });

  test("different kind, message, details or action label never collapse", () => {
    const store = createToastStore({ clock: createFakeClock(), maxVisible: 10 });
    const action = { label: "Retry", run: () => undefined };

    store.show({ kind: "info", message: "same" });
    store.show({ kind: "success", message: "same" });
    store.show({ kind: "info", message: "same", details: "trace" });
    store.show({ kind: "info", message: "same", action });

    expect(store.getSnapshot().visible).toHaveLength(4);
    expect(store.getSnapshot().visible.map((toast) => toast.count)).toEqual([1, 1, 1, 1]);
  });

  test("compares actions by identity, so two Retry buttons never merge", () => {
    const { store } = setup();
    store.show({ kind: "error", message: "failed", action: { label: "Retry", run: () => undefined } });
    store.show({ kind: "error", message: "failed", action: { label: "Retry", run: () => undefined } });
    expect(store.getSnapshot().visible).toHaveLength(2);

    const shared = createToastStore({ clock: createFakeClock(), maxVisible: 10 });
    const retry = { label: "Retry", run: () => undefined };
    shared.show({ kind: "error", message: "failed", action: retry });
    shared.show({ kind: "error", message: "failed", action: retry });
    expect(shared.getSnapshot().visible).toHaveLength(1);
    expect(shared.getSnapshot().visible[0]?.count).toBe(2);
  });

  test("collapsing restarts the auto-dismiss timer", () => {
    const { clock, store } = setup();
    store.show(info("saved"));
    clock.advance(AUTO_DISMISS_MS - 500);
    store.show(info("saved"));

    clock.advance(AUTO_DISMISS_MS - 1);
    expect(store.getSnapshot().visible).toHaveLength(1);

    clock.advance(1);
    expect(store.getSnapshot().visible).toHaveLength(0);
  });

  test("identical errors collapse and still never expire", () => {
    const { clock, store } = setup();
    store.show({ kind: "error", message: "boom" });
    store.show({ kind: "error", message: "boom" });
    clock.advance(AUTO_DISMISS_MS * 10);

    const visible = store.getSnapshot().visible;
    expect(visible).toHaveLength(1);
    expect(visible[0]?.count).toBe(2);
  });
});

describe("actions, dismissal and subscription", () => {
  test("activating a toast runs its action once and dismisses it", () => {
    const { store } = setup();
    const run = vi.fn();
    const id = store.show({ kind: "success", message: "copied", action: { label: "Undo", run } });

    store.activate(id);
    expect(run).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot().visible).toHaveLength(0);
  });

  test("activating a toast without an action still dismisses it", () => {
    const { store } = setup();
    const id = store.show(info("hello"));
    store.activate(id);
    expect(store.getSnapshot().visible).toHaveLength(0);
  });

  test("dismissAll clears the visible stack and the waiting queue", () => {
    const { clock, store } = setup();
    for (const message of ["a", "b", "c", "d"]) {
      store.show(info(message));
    }
    store.dismissAll();

    expect(store.getSnapshot()).toEqual({ visible: [], queued: 0 });
    expect(clock.pending()).toBe(0);
    clock.advance(AUTO_DISMISS_MS * 2);
    expect(store.getSnapshot().visible).toHaveLength(0);
  });

  test("notifies subscribers only while subscribed and keeps snapshots stable", () => {
    const { store } = setup();
    const listener = vi.fn();
    const unsubscribe = store.subscribe(listener);

    const before = store.getSnapshot();
    expect(store.getSnapshot()).toBe(before);

    store.show(info("hello"));
    expect(listener).toHaveBeenCalledTimes(1);
    expect(store.getSnapshot()).not.toBe(before);

    unsubscribe();
    store.dismissAll();
    expect(listener).toHaveBeenCalledTimes(1);
  });

  test("dismissing an unknown id is a no-op", () => {
    const { store } = setup();
    const id = store.show(info("hello"));
    store.dismiss("missing");
    expect(visibleIds(store)).toEqual([id]);
  });
});
