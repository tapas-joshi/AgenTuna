import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import { McpFilesThatWillChangePanel } from "./McpFilesThatWillChangePanel";
afterEach(cleanup);

const preview: McpChangeSetPreviewView = {
  change_set_id: "change-1",
  files: [
    { agent: "Claude Code", scope: "user", path: "~/.claude.json", additions: 4n, deletions: 2n, diff: "--- a/claude.json\n+++ b/claude.json\n@@ -1 +1 @@\n-old\n+new", selected: true, cannot_write_reason: null },
    { agent: "Codex", scope: "user", path: "~/.codex/config.toml", additions: 0n, deletions: 0n, diff: "", selected: true, cannot_write_reason: "Permission denied" },
  ],
};

describe("McpFilesThatWillChangePanel", () => {
  it("shows exact per-agent preview and disables save until blocked agent is deselected", () => {
    const onAgentSelectedChange = vi.fn();
    const { rerender } = render(<McpFilesThatWillChangePanel preview={preview} onAgentSelectedChange={onAgentSelectedChange} onSave={() => undefined} />);
    expect(screen.getByText("Backup created before files are replaced.")).toBeTruthy();
    expect(screen.getByText("Permission denied")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Codex" }));
    expect(onAgentSelectedChange).toHaveBeenCalledWith("Codex", false);
    const deselected = { ...preview, files: preview.files.map((file) => file.agent === "Codex" ? { ...file, selected: false } : file) };
    rerender(<McpFilesThatWillChangePanel preview={deselected} onAgentSelectedChange={onAgentSelectedChange} onSave={() => undefined} />);
    expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("opens each file diff and reports its line counts", () => {
    render(<McpFilesThatWillChangePanel preview={{ ...preview, files: [preview.files[0]!] }} onAgentSelectedChange={() => undefined} />);
    expect(screen.getAllByText("+4 −2")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Show diff for Claude Code" }));
    expect(screen.getByText("new")).toBeTruthy();
  });
});
