import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { moveRefusedItemAside, publishRefusedOverwrite } from "./overwrite-commands";

afterEach(() => {
  vi.clearAllMocks();
});

describe("overwrite command bridge", () => {
  it("invokes the registered at-fs command and exposes its preserved destination", async () => {
    invoke.mockResolvedValue({ original: "/sandbox/agent/skill", moved_to: "/sandbox/agent/skill.agentuna-backup-20261106T100000Z" });
    await expect(moveRefusedItemAside("/sandbox/agent/skill")).resolves.toEqual({
      summary: "The unmanaged item was moved aside without replacing its contents.",
      movedTo: "/sandbox/agent/skill.agentuna-backup-20261106T100000Z",
    });
    expect(invoke).toHaveBeenCalledWith("overwrite_move_aside", { path: "/sandbox/agent/skill" });
  });

  it("publishes only typed refusal payloads for the global dialog host", () => {
    const listener = vi.fn();
    window.addEventListener("agentuna:refused-overwrite", listener);
    expect(publishRefusedOverwrite({ code: "refused_overwrite", refusal: { path: "/sandbox/item" } })).toBe(true);
    expect(listener).toHaveBeenCalledOnce();
    expect(publishRefusedOverwrite({ code: "internal", message: "failure" })).toBe(false);
    expect(listener).toHaveBeenCalledOnce();
    window.removeEventListener("agentuna:refused-overwrite", listener);
  });
});
