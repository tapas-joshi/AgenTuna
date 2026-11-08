import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { AdoptModal } from "./AdoptModal";

const MOCK_INVOKE = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => MOCK_INVOKE(...args),
}));

describe("AdoptModal", () => {
  beforeEach(() => {
    MOCK_INVOKE.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  test("loads adopt preview and confirms adoption on click", async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    const onClose = vi.fn();

    MOCK_INVOKE.mockImplementation((command: string) => {
      if (command === "adopt_preview") {
        return Promise.resolve({
          source: "/home/.agents/skills/release-notes",
          store_root: "/home/.agentuna",
          store_destination: "/home/.agentuna/skills/release-notes",
          backup_dir: "/home/.agentuna/backups",
          slug: "release-notes",
          native_name: "release-notes",
          kind: "skill",
          agents: ["codex", "omp"],
          handling: "link",
          renamed: false,
          current: "present",
          hash: "sha256-previewed-bytes",
        });
      }
      if (command === "adopt_apply") {
        return Promise.resolve({
          summary: "Adopted release-notes as release-notes (link).",
          operation_id: "op_adopt_123",
          store_destination: "/home/.agentuna/skills/release-notes",
          slug: "release-notes",
          handling: "link",
        });
      }
      return Promise.reject(new Error("unexpected command"));
    });

    render(
      <AdoptModal
        path="/home/.agents/skills/release-notes"
        onAdoptSuccess={onSuccess}
        onClose={onClose}
      />,
    );

    expect(await screen.findByText("Adopt release-notes")).toBeTruthy();
    expect(screen.getByText("/home/.agentuna/skills/release-notes")).toBeTruthy();
    expect(screen.getByText("/home/.agentuna/backups")).toBeTruthy();
    expect(screen.getAllByText(/codex \+ omp/).length).toBeGreaterThanOrEqual(1);

    await user.click(screen.getByRole("button", { name: "Adopt" }));
    await waitFor(() => {
      expect(onSuccess).toHaveBeenCalledWith(
        expect.objectContaining({
          operation_id: "op_adopt_123",
        }),
      );
    });
    // The write carries the identity of the preview the user reviewed, so a source that
    // changed between review and apply is refused instead of adopted.
    expect(MOCK_INVOKE).toHaveBeenCalledWith("adopt_apply", {
      path: "/home/.agents/skills/release-notes",
      name: null,
      expectedHash: "sha256-previewed-bytes",
      handling: "link",
    });
    expect(onClose).toHaveBeenCalled();
  });

  test("shows stale scan error and offers rescan button", async () => {
    const user = userEvent.setup();
    const onRescan = vi.fn();
    const onClose = vi.fn();

    MOCK_INVOKE.mockRejectedValueOnce("StaleScanRecord: file changed on disk");

    render(
      <AdoptModal
        path="/home/.claude/skills/stale"
        onAdoptSuccess={vi.fn()}
        onRescan={onRescan}
        onClose={onClose}
      />,
    );

    expect(await screen.findByRole("alert")).toBeTruthy();
    const rescanBtn = screen.getByRole("button", { name: "Rescan agent folders" });
    expect(rescanBtn).toBeTruthy();

    await user.click(rescanBtn);
    expect(onRescan).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("renders a typed command error message instead of object coercion", async () => {
    MOCK_INVOKE.mockRejectedValueOnce({
      code: "NotFound",
      message: "The native item could not be found.",
      remedy: "Rescan agent folders.",
    });

    render(
      <AdoptModal
        path="/sandbox/.codex/skills/codex-active"
        onAdoptSuccess={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(await screen.findByText("The native item could not be found.")).toBeTruthy();
    expect(screen.queryByText("[object Object]")).toBeNull();
  });
});
