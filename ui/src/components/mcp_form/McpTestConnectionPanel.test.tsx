import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { McpTestResultView } from "../../generated/McpTestResultView";
import { McpTestConnectionPanel } from "./McpTestConnectionPanel";
afterEach(cleanup);

describe("McpTestConnectionPanel", () => {
  it("shows an honest empty tools state and runs only after explicit test click", () => {
    const onRunTest = vi.fn();
    render(<McpTestConnectionPanel result={null} onRunTest={onRunTest} />);
    expect(screen.getByText("Tools appear after a successful test.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(onRunTest).toHaveBeenCalledOnce();
  });
  it("runs only on explicit click and shows first five tool names with expansion", () => {
    const onRunTest = vi.fn();
    const result: McpTestResultView = {
      ok: true,
      server_name: "Example",
      server_version: null,
      transport: "stdio",
      latency_ms: 12n,
      tools_count: 6n,
      tools: ["one", "two", "three", "four", "five", "six"],
      error: null,
      stderr_excerpt: null,
    };
    render(<McpTestConnectionPanel result={result} onRunTest={onRunTest} />);
    expect(onRunTest).not.toHaveBeenCalled();
    expect(screen.getByText("Connected")).toBeTruthy();
    expect(screen.getByText("+1 more")).toBeTruthy();
    expect(screen.queryByText("six")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show all tools" }));
    expect(screen.getByText("six")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(onRunTest).toHaveBeenCalledOnce();
  });

  it("shows running state with cancel action and a stale result badge", () => {
    const onCancel = vi.fn();
    render(<McpTestConnectionPanel result={null} running elapsedSeconds={4} stale onCancel={onCancel} onRunTest={() => undefined} />);
    expect(screen.getByText("Test result is stale")).toBeTruthy();
    expect(screen.getByText("Testing… 4s")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel test" }));
    expect(onCancel).toHaveBeenCalledOnce();
  });

  it("renders redacted error details and invokes the supplied suggested action", () => {
    const onSuggestedAction = vi.fn();
    const result: McpTestResultView = {
      ok: false,
      server_name: "Example",
      server_version: null,
      transport: "http",
      latency_ms: 3n,
      tools_count: 0n,
      tools: [],
      error: { kind: "sign_in_required", message: "Sign-in required", suggested_action: "Sign in" },
      stderr_excerpt: "server diagnostic",
    };
    render(<McpTestConnectionPanel result={result} onSuggestedAction={onSuggestedAction} onRunTest={() => undefined} />);
    expect(screen.getByText("Sign-in required")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(onSuggestedAction).toHaveBeenCalledWith("Sign in");
    expect(screen.getByText("Show server output")).toBeTruthy();
  });
});
