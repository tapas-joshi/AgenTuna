import { StrictMode } from "react";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AsyncBoundary } from "./async-boundary";
import { useAsyncResource, type AsyncResourceOptions } from "./use-async-resource";

afterEach(cleanup);

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
  readonly reject: (reason: unknown) => void;
}

function deferred<T>(): Deferred<T> {
  const { promise, resolve, reject } = Promise.withResolvers<T>();
  return { promise, resolve, reject };
}

interface HarnessProps {
  readonly load: () => Promise<string[]>;
  readonly deps?: readonly unknown[];
  readonly minLoadingMs?: number;
  readonly isEmpty?: AsyncResourceOptions<string[]>["isEmpty"];
}

function Harness({ load, deps = [], minLoadingMs = 0, isEmpty }: HarnessProps) {
  const resource = useAsyncResource(load, { deps, minLoadingMs, isEmpty });
  return (
    <AsyncBoundary
      state={resource}
      skeletonRows={3}
      loadingTitle="Loading skills…"
      minHeight={240}
      empty={{
        title: "No skills in this scope",
        body: "Import a skill or create one from scratch.",
        action: { label: "Add skill", onClick: () => {} },
      }}
    >
      {(rows) => (
        <ul>
          {rows.map((row) => (
            <li key={row}>{row}</li>
          ))}
        </ul>
      )}
    </AsyncBoundary>
  );
}

describe("useAsyncResource with AsyncBoundary", () => {
  test("first paint is loading, never an empty flash", async () => {
    render(<Harness load={async () => ["tdd", "code-review"]} />);

    expect(screen.getByRole("heading", { name: "Loading skills…" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "No skills in this scope" })).toBeNull();
    expect(document.querySelectorAll('[data-slot="skeleton-row"]')).toHaveLength(3);

    expect(await screen.findByText("tdd")).toBeTruthy();
    expect(screen.getByText("code-review")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Loading skills…" })).toBeNull();
  });

  test("an empty result renders the empty state with exactly one action", async () => {
    render(<Harness load={async () => []} />);

    expect(await screen.findByRole("heading", { name: "No skills in this scope" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add skill" })).toBeTruthy();
    expect(screen.queryByRole("list")).toBeNull();
  });

  test("a rejection renders the mapped error and retry keeps it visible until data arrives", async () => {
    const user = userEvent.setup();
    const retryResult = deferred<string[]>();
    const load = vi
      .fn<() => Promise<string[]>>()
      .mockRejectedValueOnce(new Error("backend unavailable"))
      .mockReturnValueOnce(retryResult.promise);

    render(<Harness load={load} />);

    expect(await screen.findByRole("heading", { name: "Something went wrong" })).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Retry" }));

    expect(screen.getByRole("heading", { name: "Something went wrong" })).toBeTruthy();
    const retry = screen.getByRole("button", { name: "Retry" });
    expect(retry.getAttribute("aria-busy")).toBe("true");

    await act(async () => {
      retryResult.resolve(["tdd"]);
    });

    expect(await screen.findByText("tdd")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Something went wrong" })).toBeNull();
  });

  test("StrictMode's double effect issues one read, not two", async () => {
    const load = vi.fn(async () => ["tdd"]);

    render(
      <StrictMode>
        <Harness load={load} deps={["project"]} />
      </StrictMode>,
    );

    expect(await screen.findByText("tdd")).toBeTruthy();
    expect(load).toHaveBeenCalledTimes(1);
  });

  test("the loading state is held for the minimum time so it never flashes", async () => {
    vi.useFakeTimers();
    try {
      render(<Harness load={async () => ["tdd"]} minLoadingMs={200} />);

      await act(async () => {
        await Promise.resolve();
      });
      expect(screen.getByRole("heading", { name: "Loading skills…" })).toBeTruthy();

      await act(async () => {
        vi.advanceTimersByTime(199);
      });
      expect(screen.queryByText("tdd")).toBeNull();

      await act(async () => {
        vi.advanceTimersByTime(2);
      });
      expect(screen.queryByText("tdd")).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  test("a superseded attempt never overwrites the newer one", async () => {
    const first = deferred<string[]>();
    const second = deferred<string[]>();
    const load = vi
      .fn<() => Promise<string[]>>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    const { rerender } = render(<Harness load={load} deps={["project"]} />);
    rerender(<Harness load={load} deps={["global"]} />);

    await act(async () => {
      first.resolve(["stale"]);
      second.resolve(["global-skill"]);
    });

    expect(screen.queryByText("stale")).toBeNull();
    expect(screen.getByText("global-skill")).toBeTruthy();
  });

  test("a reload with data on screen keeps the data and marks the region busy", async () => {
    const reloaded = deferred<string[]>();
    const load = vi
      .fn<() => Promise<string[]>>()
      .mockResolvedValueOnce(["tdd"])
      .mockReturnValueOnce(reloaded.promise);

    const { rerender } = render(<Harness load={load} deps={["user"]} />);
    expect(await screen.findByText("tdd")).toBeTruthy();

    rerender(<Harness load={load} deps={["project"]} />);

    expect(screen.getByText("tdd")).toBeTruthy();
    const wrapper = document.querySelector('[data-slot="async-boundary"]');
    expect(wrapper?.getAttribute("aria-busy")).toBe("true");

    await act(async () => {
      reloaded.resolve(["tdd", "code-review"]);
    });
    expect(screen.getByText("code-review")).toBeTruthy();
  });

  test("a custom empty predicate decides when the empty state wins", async () => {
    render(
      <Harness
        load={async () => ["placeholder"]}
        isEmpty={(rows) => rows.length === 1 && rows[0] === "placeholder"}
      />,
    );

    expect(await screen.findByRole("heading", { name: "No skills in this scope" })).toBeTruthy();
  });

  test("every state reserves the same height", async () => {
    const { rerender } = render(<Harness load={async () => []} minLoadingMs={0} deps={["first"]} />);
    const loadingRoot = document.querySelector<HTMLElement>('[data-slot="state-view"]');
    expect(loadingRoot?.style.minHeight).toBe("240px");

    await screen.findByRole("heading", { name: "No skills in this scope" });
    const emptyRoot = document.querySelector<HTMLElement>('[data-slot="state-view"]');
    expect(emptyRoot?.style.minHeight).toBe("240px");

    rerender(<Harness load={async () => Promise.reject(new Error("boom"))} deps={["second"]} />);
    expect(await screen.findByRole("heading", { name: "Something went wrong" })).toBeTruthy();
    const errorRoot = document.querySelector<HTMLElement>('[data-slot="state-view"]');
    expect(errorRoot?.style.minHeight).toBe("240px");
  });
});
