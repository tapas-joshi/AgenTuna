import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { ModalHost, useModals } from "./ModalHost";
import { createModalQueue } from "./modal-queue";
import type { ModalResult } from "./types";

afterEach(cleanup);

/** One recorded settlement, flattened so assertions stay outside React batching. */
interface OutcomeSummary {
  readonly kind: ModalResult<unknown>["kind"];
  readonly value?: unknown;
}

function outcomeOf(result: ModalResult<unknown>): OutcomeSummary {
  return result.kind === "confirmed" ? { kind: result.kind, value: result.value } : { kind: result.kind };
}

interface HarnessProps {
  readonly onResult: (outcome: OutcomeSummary) => void;
  readonly destructive?: boolean;
}

function Harness({ onResult, destructive = false }: HarnessProps) {
  const modals = useModals();

  return (
    <>
      <button
        type="button"
        onClick={async () => {
          onResult(
            outcomeOf(
              await modals.open<string>({
                title: "Adopt release-notes",
                description: "Review the original and the destination first.",
                dismissible: !destructive,
                confirm: { label: "Adopt", value: "adopt" },
                body: () => (
                  <>
                    <p>Original is copied, then replaced by a link.</p>
                    <button
                      type="button"
                      onClick={async () => {
                        onResult(
                          outcomeOf(
                            await modals.open<string>({
                              title: "Nested request",
                              body: () => <p>Nested body</p>,
                            }),
                          ),
                        );
                      }}
                    >
                      Request nested
                    </button>
                  </>
                ),
              }),
            ),
          );
        }}
      >
        Request adopt
      </button>
      <button
        type="button"
        onClick={async () => {
          onResult(
            outcomeOf(
              await modals.open<string>({
                title: "Body action",
                body: (api) => (
                  <button type="button" onClick={() => api.confirm("picked")}>
                    Pick release-notes
                  </button>
                ),
              }),
            ),
          );
        }}
      >
        Request body action
      </button>
    </>
  );
}

describe("ModalHost", () => {
  test("the confirm button resolves the promise with its typed value", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    const dialog = screen.getByRole("dialog", { name: "Adopt release-notes" });
    expect(dialog.textContent).toContain("Original is copied, then replaced by a link.");

    await user.click(screen.getByRole("button", { name: "Adopt" }));

    await waitFor(() => expect(outcomes).toEqual([{ kind: "confirmed", value: "adopt" }]));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  test("the cancel button resolves cancelled, not dismissed", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(outcomes).toEqual([{ kind: "cancelled" }]));
  });

  test("Escape resolves dismissed", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    await user.keyboard("{Escape}");

    await waitFor(() => expect(outcomes).toEqual([{ kind: "dismissed" }]));
  });

  test("a destructive request ignores Escape until the user chooses", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness destructive onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    await user.keyboard("{Escape}");

    expect(outcomes).toEqual([]);
    expect(screen.getByRole("dialog", { name: "Adopt release-notes" })).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(outcomes).toEqual([{ kind: "cancelled" }]));
  });

  test("a body action confirms a value the body computed", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request body action" }));
    await user.click(screen.getByRole("button", { name: "Pick release-notes" }));

    await waitFor(() => expect(outcomes).toEqual([{ kind: "confirmed", value: "picked" }]));
  });

  test("stacked requests close top first and each promise settles once", async () => {
    const user = userEvent.setup();
    const outcomes: OutcomeSummary[] = [];
    render(
      <ModalHost>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    await user.click(screen.getByRole("button", { name: "Request nested" }));
    expect(document.querySelectorAll(".at-modal")).toHaveLength(2);

    await user.keyboard("{Escape}");
    await waitFor(() =>
      expect(document.querySelectorAll(".at-modal")).toHaveLength(1),
    );
    expect(document.querySelector(".at-modal")?.textContent).toContain("Adopt release-notes");
    expect(outcomes).toEqual([{ kind: "dismissed" }]);

    await user.keyboard("{Escape}");
    await waitFor(() =>
      expect(document.querySelectorAll(".at-modal")).toHaveLength(0),
    );
    expect(outcomes).toEqual([{ kind: "dismissed" }, { kind: "dismissed" }]);
  });

  test("unmounting the host settles pending requests as dismissed", async () => {
    const user = userEvent.setup();
    const queue = createModalQueue();
    const outcomes: OutcomeSummary[] = [];
    const view = render(
      <ModalHost queue={queue}>
        <Harness onResult={(outcome) => outcomes.push(outcome)} />
      </ModalHost>,
    );

    await user.click(screen.getByRole("button", { name: "Request adopt" }));
    expect(queue.getSnapshot().entries).toHaveLength(1);

    view.unmount();

    await waitFor(() => expect(outcomes).toEqual([{ kind: "dismissed" }]));
    expect(queue.getSnapshot().entries).toEqual([]);
  });

  test("useModals outside a host fails loudly instead of hanging", () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    function Consumer() {
      useModals();
      return null;
    }

    expect(() => render(<Consumer />)).toThrow(/useModals must be called inside <ModalHost>/);
    error.mockRestore();
  });
});
