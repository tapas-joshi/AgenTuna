import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import type { McpLaunchApprovalView } from "../../generated/McpLaunchApprovalView";
import type { McpTestResultView } from "../../generated/McpTestResultView";
import { AddMcpServerForm } from "./AddMcpServerForm";
if (typeof globalThis.ResizeObserver === "undefined") {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;
}

afterEach(cleanup);

const preview: McpChangeSetPreviewView = {
  change_set_id: "plan-1",
  preview_id: "preview-1",
  files: [{ agent: "Codex", scope: "user", path: "/sandbox/.codex/config.toml", additions: 2n, deletions: 0n, diff: "+++ config.toml\n+server", selected: true, cannot_write_reason: null }],
  unsupported_fields: [],
  warnings: [],
  store_init: null,
};

describe("AddMcpServerForm", () => {
  it("validates required fields without requesting a plan", async () => {
    const onPlan = vi.fn();
    render(<AddMcpServerForm onCancel={() => undefined} onPlan={onPlan} onSave={() => Promise.resolve()} />);
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    expect(await screen.findByText("Enter a server name.")).toBeTruthy();
    expect(await screen.findByText("Enter a command.")).toBeTruthy();
    expect(await screen.findByText("Select at least one installed agent.")).toBeTruthy();
    expect(onPlan).not.toHaveBeenCalled();
  });

  it("preserves stdio values across transports and saves only after reviewing the planner preview", async () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={onPlan} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "HTTP" }));
    fireEvent.change(screen.getByLabelText("URL"), { target: { value: "https://example.invalid/mcp" } });
    fireEvent.click(screen.getByRole("button", { name: "Command (stdio)" }));
    expect((screen.getByLabelText("Command") as HTMLInputElement).value).toBe("npx");
    fireEvent.click(screen.getByRole("button", { name: "Add variable" }));
    fireEvent.change(screen.getByLabelText("Variable 1 name"), { target: { value: "API_KEY" } });
    fireEvent.change(screen.getByLabelText("Variable 1 value type"), { target: { value: "literal" } });
    fireEvent.change(screen.getByLabelText("Variable 1 literal value"), { target: { value: "fixture-value" } });
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    await waitFor(() => expect(onPlan).toHaveBeenCalledTimes(1));
    expect(onSave).not.toHaveBeenCalled();
    expect(await screen.findByText("Files that will change")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^Save to / }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("preview-1", ["Codex"], false));
    expect(onPlan.mock.calls[0]?.[0]).toMatchObject({ name: "Search", command: "npx", url: "https://example.invalid/mcp" });
    expect(await screen.findByText("MCP server saved.")).toBeTruthy();
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("");
  });

  it("shows the Global consent remedy when the plan is refused", async () => {
    const message = "Enable Global writes before this plan can create /sandbox/global. Open Overview, choose the Global scope and use Enable global writes.";
    const onPlan = vi.fn().mockRejectedValue(new Error(message));
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={onPlan} onSave={() => Promise.resolve()} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Docs" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    expect(await screen.findByText(/Enable Global writes before this plan/)).toBeTruthy();
    expect(screen.getByText(/use Enable global writes/)).toBeTruthy();
  });

  it("shows duplicate ownership, scope permissions and not-installed agents", () => {
    render(<AddMcpServerForm installedAgents={["codex"]} globalAllowed={false} duplicateScope="project" onCancel={() => undefined} onPlan={() => Promise.resolve(preview)} onSave={() => Promise.resolve()} />);
    expect(screen.getByText(/already exists in project scope/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Review changes" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Claude Code" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Codex" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByLabelText("Global") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getAllByText("not installed").length).toBe(3);
    expect(screen.getByText("Local (only you, this repo)")).toBeTruthy();
  });
  it("prefills edit values and disables save until dirty", () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    render(<AddMcpServerForm mode="edit" initialState={{
      id: "itm_fixture", name: "github", transport: "stdio", command: "node", args: ["server.js"], url: "",
      headers: [], env: [{ name: "TOKEN", value: "GITHUB_TOKEN", kind: "secret" }],
      scope: "user", agents: ["codex"],
    }} onCancel={() => undefined} onPlan={onPlan} onSave={() => Promise.resolve()} />);
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("github");
    expect((screen.getByLabelText("Command") as HTMLInputElement).value).toBe("node");
    expect((screen.getByRole("button", { name: "Review edit" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "node-new" } });
    expect((screen.getByRole("button", { name: "Review edit" }) as HTMLButtonElement).disabled).toBe(false);
  });

  const editInitial = {
    id: "itm_fixture", name: "github", transport: "stdio" as const, command: "node", args: ["server.js"], url: "",
    headers: [], env: [], scope: "user" as const, agents: ["codex"] as const,
  };
  const connected: McpTestResultView = {
    ok: true, server_name: "Search", server_version: null, transport: "stdio", latency_ms: 12n,
    tools_count: 0n, tools: [], error: null, stderr_excerpt: null,
  };

  const approvalView: McpLaunchApprovalView = {
    approval_id: "apr-fixture", request_hash: "a".repeat(64), transport: "stdio",
    command: "npx", args: [], cwd: null, value_names: [], url: null, expires_in_ms: 120000n,
  };
  const confirmRun = async () => {
    const dialog = await screen.findByRole("dialog", { name: "Run this connection test?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Run" }));
  };

  it("asks for confirmation of the exact launch before starting the test", async () => {
    const onTest = vi.fn().mockResolvedValue(connected);
    const onPrepareLaunch = vi.fn().mockResolvedValue({ ...approvalView, args: ["--serve"], cwd: "/sandbox", value_names: ["TOKEN"] });
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={vi.fn().mockResolvedValue(preview)} onSave={vi.fn().mockResolvedValue(undefined)} onPrepareLaunch={onPrepareLaunch} onTest={onTest} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    const dialog = await screen.findByRole("dialog", { name: "Run this connection test?" });
    expect(dialog.textContent).toContain("npx --serve");
    expect(dialog.textContent).toContain("/sandbox");
    expect(dialog.textContent).toContain("TOKEN");
    expect(onTest).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog", { name: "Run this connection test?" })).toBeNull();
    expect(onTest).not.toHaveBeenCalled();
    expect(onPrepareLaunch).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    await confirmRun();
    await waitFor(() => expect(onTest).toHaveBeenCalledTimes(1));
    expect(onTest.mock.calls[0]?.[1]).toEqual({ approval_id: "apr-fixture", request_hash: "a".repeat(64) });
  });

  it("clears a successful test result when a field changes", async () => {
    const onTest = vi.fn().mockResolvedValue(connected);
    const onPrepareLaunch = vi.fn().mockResolvedValue(approvalView);
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={vi.fn().mockResolvedValue(preview)} onSave={vi.fn().mockResolvedValue(undefined)} onPrepareLaunch={onPrepareLaunch} onTest={onTest} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    await confirmRun();
    expect(await screen.findByText("Connected")).toBeTruthy();
    expect(screen.queryByText("Test result is stale")).toBeNull();
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search two" } });
    expect(screen.getByText("Test result is stale")).toBeTruthy();
    expect(screen.queryByText("Connected")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    await confirmRun();
    await waitFor(() => expect(onTest).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("Connected")).toBeTruthy();
    expect(screen.queryByText("Test result is stale")).toBeNull();
  });

  it("shows the redacted failure and Sign in action from the draft test", async () => {
    const onSuggestedAction = vi.fn();
    const onTest = vi.fn().mockResolvedValue({
      ...connected, ok: false,
      error: { kind: "sign_in_required", message: "Sign-in required", suggested_action: "Sign in" },
    });
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={vi.fn().mockResolvedValue(preview)} onSave={vi.fn().mockResolvedValue(undefined)} onPrepareLaunch={vi.fn().mockResolvedValue(approvalView)} onTest={onTest} onSuggestedAction={onSuggestedAction} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    await confirmRun();
    expect(await screen.findByText("Sign-in required")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(onSuggestedAction).toHaveBeenCalledWith("Sign in");
  });

  it("confirms a rename and applies nothing before the explicit Apply", async () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<AddMcpServerForm mode="edit" initialState={editInitial} onCancel={() => undefined} onPlan={onPlan} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "github-enterprise" } });
    fireEvent.click(screen.getByRole("button", { name: "Review edit" }));
    await screen.findByText("Files that will change");
    fireEvent.click(screen.getByRole("button", { name: /^Save to / }));
    const dialog = screen.getByRole("dialog", { name: /Rename/ });
    expect(dialog.textContent).toContain("github-enterprise");
    expect(dialog.textContent).toContain("/sandbox/.codex/config.toml");
    expect(onSave).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog", { name: /Rename/ })).toBeNull();
    expect(onSave).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /^Save to / }));
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("preview-1", ["Codex"], false));
  });

  it("requires acknowledgement of dropped fields before applying", async () => {
    const unsupported = { ...preview, unsupported_fields: [{ agent: "codex", fields: ["headers"] }] };
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={vi.fn().mockResolvedValue(unsupported)} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    await screen.findByText(/will be dropped/);
    expect((screen.getByRole("button", { name: /^Save to / }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "Acknowledge dropped fields" }));
    expect((screen.getByRole("button", { name: /^Save to / }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: /^Save to / }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith("preview-1", ["Codex"], true));
  });

  it("shows the external-write conflict dialog and re-plans only on a click", async () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    const onSave = vi.fn().mockRejectedValue({ code: "external_write_conflict", message: "a reviewed file changed" });
    render(<AddMcpServerForm defaultAgents={["codex"]} onCancel={() => undefined} onPlan={onPlan} onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Search" } });
    fireEvent.change(screen.getByLabelText("Command"), { target: { value: "npx" } });
    fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
    await screen.findByText("Files that will change");
    fireEvent.click(screen.getByRole("button", { name: /^Save to / }));
    const dialog = await screen.findByRole("dialog", { name: /changed outside AgenTuna/ });
    expect(within(dialog).getByRole("button", { name: "Keep mine (re-apply)" })).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(onPlan).toHaveBeenCalledTimes(1);
    fireEvent.click(within(dialog).getByRole("button", { name: "Keep mine (re-apply)" }));
    await waitFor(() => expect(onPlan).toHaveBeenCalledTimes(2));
  });

  it("does not plan an unchanged edit", () => {
    const onPlan = vi.fn().mockResolvedValue(preview);
    render(<AddMcpServerForm mode="edit" initialState={editInitial} onCancel={() => undefined} onPlan={onPlan} onSave={vi.fn().mockResolvedValue(undefined)} />);
    const review = screen.getByRole("button", { name: "Review edit" }) as HTMLButtonElement;
    expect(review.disabled).toBe(true);
    fireEvent.click(review);
    expect(onPlan).not.toHaveBeenCalled();
  });
});
