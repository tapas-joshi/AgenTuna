import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { LogViewer } from "./LogViewer";

const ipc = vi.hoisted(() => ({ calls: [] as string[], reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string) => { ipc.calls.push(command); return ipc.reply(command); } }));
const fixture = { path: "/sandbox/logs/agentuna-2026-11-06.log", lines: [
  { level: "ERROR", target: "svc=fs", text: "concurrent modification in file A" },
  { level: "ERROR", target: "svc=network", text: "concurrent socket failure" },
  { level: "WARN", target: "svc=fs", text: "concurrent retry scheduled" },
  { level: "INFO", target: "svc=fs", text: "read complete" },
] };
afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("LogViewer", () => {
  test("combines severity, module and substring filters and reports hidden count", async () => {
    ipc.reply = () => Promise.resolve(fixture);
    render(<LogViewer />);
    fireEvent.change(await screen.findByLabelText("Level filter"), { target: { value: "ERROR" } });
    fireEvent.change(screen.getByLabelText("Module filter"), { target: { value: "svc=fs" } });
    fireEvent.change(screen.getByLabelText("Search log text"), { target: { value: "concurrent" } });
    expect(screen.getByText("concurrent modification in file A")).toBeTruthy();
    expect(screen.queryByText("concurrent socket failure")).toBeNull();
    expect(screen.getByText("1 shown · 3 hidden")).toBeTruthy();
  });

  test("missing log file exposes a path and Retry re-reads without restarting", async () => {
    ipc.reply = () => Promise.reject("file not found: /sandbox/logs/current.log");
    render(<LogViewer />);
    expect(await screen.findByText(/file not found/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(ipc.calls).toEqual(["logs_tail", "logs_tail"]);
  });

  test("empty log directory shows the resolved path without an error", async () => {
    ipc.reply = () => Promise.resolve({ path: "/sandbox/logs/agentuna-*.log", lines: [] });
    render(<LogViewer />);
    expect(await screen.findByText("No log entries yet")).toBeTruthy();
    expect(screen.getByText("Log file: /sandbox/logs/agentuna-*.log")).toBeTruthy();
  });
});
