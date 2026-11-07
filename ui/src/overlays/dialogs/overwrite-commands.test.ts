import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { applyAdoption, moveRefusedItemAside, previewAdoption, publishRefusedOverwrite } from "./overwrite-commands";

afterEach(() => {
  vi.clearAllMocks();
});

describe("overwrite command bridge", () => {
  it("invokes the registered at-fs command and exposes its preserved destination", async () => {
    invoke.mockResolvedValue({ original: "/sandbox/agent/skill", moved_to: "/sandbox/agent/skill.agentuna-backup-20261106T100000Z", operation_id: "op_move_1" });
    await expect(moveRefusedItemAside("/sandbox/agent/skill")).resolves.toEqual({
      summary: "The unmanaged item was moved aside without replacing its contents.",
      movedTo: "/sandbox/agent/skill.agentuna-backup-20261106T100000Z",
      operationId: "op_move_1",
      remedy: "move",
    });
    expect(invoke).toHaveBeenCalledWith("overwrite_move_aside", { path: "/sandbox/agent/skill" });
  });

  it("invokes the adopt preview and apply commands with the refusal hash and handling", async () => {
    invoke.mockResolvedValueOnce({ source: "/sandbox/agent/skill", store_destination: "/sandbox/user/skills/skill", slug: "skill", native_name: "skill", kind: "skill", agents: ["claude_code"], store_root: "/sandbox/user", handling: "link", renamed: false, current: "present" });
    await previewAdoption("/sandbox/agent/skill", "abc123", "link");
    expect(invoke).toHaveBeenLastCalledWith("adopt_preview", { path: "/sandbox/agent/skill", expectedHash: "abc123", handling: "link" });

    invoke.mockResolvedValueOnce({ operation_id: "op_1", store_destination: "/sandbox/user/skills/skill", slug: "skill", handling: "copy", summary: "Adopted." });
    await applyAdoption("/sandbox/agent/skill", "abc123", "copy");
    expect(invoke).toHaveBeenLastCalledWith("adopt_apply", { path: "/sandbox/agent/skill", expectedHash: "abc123", handling: "copy" });
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
