import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpCopyPreviewView } from "../../generated/McpCopyPreviewView";
import { McpCopyDialog } from "./McpCopyDialog";

const preview: McpCopyPreviewView = {
  preview_id: "preview-copy",
  name: "Docs server",
  source_scope: "user",
  target_scope: "project",
  source_slug: "docs-server",
  target_slug: "docs-server",
  target_item_id: "itm_copy",
  replaces_existing: false,
  backup_location: "/sandbox/support/backups",
  files: ["/sandbox/projects/demo/.agentuna/mcp.json"],
  agents: [],
  refusals: [],
  warnings: [],
  collision: null,
  store_init: { path: "/sandbox/projects/demo/.agentuna", files_created: 6n, foreign: [] },
};

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));

afterEach(() => {
  cleanup();
  invoke.mockReset();
});

describe("McpCopyDialog", () => {
  it("shows the destination store creation the same reviewed plan performs", async () => {
    invoke.mockResolvedValueOnce(preview);
    render(
      <McpCopyDialog
        itemId="itm_source"
        scope="user"
        projectId="prj_demo"
        targetScope="project"
        onClose={() => undefined}
        onSuccess={() => undefined}
      />,
    );
    const line = await screen.findByLabelText("Store creation");
    expect(line.textContent).toContain("Create store at");
    expect(line.textContent).toContain("/sandbox/projects/demo/.agentuna");
  });
});
