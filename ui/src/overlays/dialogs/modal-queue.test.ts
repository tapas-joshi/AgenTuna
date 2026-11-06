import { describe, expect, test, vi } from "vitest";
import { createModalQueue } from "./modal-queue";
import { MODAL_CANCELLED, MODAL_DISMISSED } from "./types";


describe("createModalQueue", () => {
  test("settles confirmed with the value the caller chose", async () => {
    const queue = createModalQueue();
    const result = queue.open<{ readonly id: string }>({
      title: "Pick a skill",
      body: () => null,
    });

    const entry = queue.getSnapshot().entries[0];
    expect(entry?.title).toBe("Pick a skill");
    queue.confirm(entry?.id ?? "", { id: "release-notes" });

    await expect(result).resolves.toEqual({ kind: "confirmed", value: { id: "release-notes" } });
    expect(queue.getSnapshot().entries).toEqual([]);
  });

  test("distinguishes an explicit cancel from a dismissal", async () => {
    const queue = createModalQueue();
    const cancelled = queue.open({ title: "First", body: () => null });
    const dismissed = queue.open({ title: "Second", body: () => null });
    const [first, second] = queue.getSnapshot().entries;

    queue.cancel(first?.id ?? "");
    queue.dismiss(second?.id ?? "");

    await expect(cancelled).resolves.toBe(MODAL_CANCELLED);
    await expect(dismissed).resolves.toBe(MODAL_DISMISSED);
  });

  test("keeps requests in open order and removes only the settled one", () => {
    const queue = createModalQueue();
    void queue.open({ title: "Bottom", body: () => null });
    void queue.open({ title: "Top", body: () => null });

    expect(queue.getSnapshot().entries.map((entry) => entry.title)).toEqual(["Bottom", "Top"]);

    const top = queue.getSnapshot().entries[1];
    queue.dismiss(top?.id ?? "");

    expect(queue.getSnapshot().entries.map((entry) => entry.title)).toEqual(["Bottom"]);
  });

  test("a request settles once even when two actions race", async () => {
    const queue = createModalQueue();
    let resolutions = 0;
    const result = queue.open<number>({ title: "Race", body: () => null });
    const settled = result.then((value) => {
      resolutions += 1;
      return value;
    });

    const id = queue.getSnapshot().entries[0]?.id ?? "";
    queue.confirm(id, 1);
    queue.cancel(id);
    queue.dismiss(id);

    await expect(settled).resolves.toEqual({ kind: "confirmed", value: 1 });
    expect(resolutions).toBe(1);
  });

  test("ignores ids that are not open", () => {
    const queue = createModalQueue();
    const notify = vi.fn();
    queue.subscribe(notify);

    queue.confirm("missing", 1);
    queue.cancel("missing");
    queue.dismiss("missing");

    expect(notify).not.toHaveBeenCalled();
    expect(queue.getSnapshot().entries).toEqual([]);
  });

  test("reset dismisses everything still open for teardown", async () => {
    const queue = createModalQueue();
    const first = queue.open({ title: "First", body: () => null });
    const second = queue.open({ title: "Second", body: () => null });

    queue.reset();

    await expect(first).resolves.toBe(MODAL_DISMISSED);
    await expect(second).resolves.toBe(MODAL_DISMISSED);
    expect(queue.getSnapshot().entries).toEqual([]);
  });

  test("request options reach the host entry with defaults filled in", () => {
    const queue = createModalQueue();
    void queue.open<boolean>({
      title: "Delete skill",
      description: "This cannot be undone.",
      size: "sm",
      dismissible: false,
      cancelLabel: "Keep",
      confirm: { label: "Delete", value: true, tone: "destructive" },
      body: () => null,
    });

    const entry = queue.getSnapshot().entries[0];
    expect(entry).toMatchObject({
      title: "Delete skill",
      description: "This cannot be undone.",
      size: "sm",
      dismissible: false,
      cancelLabel: "Keep",
      confirmLabel: "Delete",
      confirmTone: "destructive",
      confirmValue: true,
    });
  });

  test("the default cancel label is Cancel and dismissal is allowed", () => {
    const queue = createModalQueue();
    void queue.open({ title: "Defaults", body: () => null });

    const entry = queue.getSnapshot().entries[0];
    expect(entry?.cancelLabel).toBe("Cancel");
    expect(entry?.dismissible).toBe(true);
    expect(entry?.confirmLabel).toBeUndefined();
  });
});
