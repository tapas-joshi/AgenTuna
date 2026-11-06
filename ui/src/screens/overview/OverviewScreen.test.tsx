import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { AppStateProvider } from "../../state/context";
import { createAppState } from "../../state/AppState";
import { OverviewScreen } from "./OverviewScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command); } }));
const appState = createAppState();
appState.restore("project-id", "project");
const payload = {
  scopes: [
    { scope: "global", skills: 1, mcp: 0, rules: 0, locks: 1, last_edited: "2026-11-05T10:00:00Z", disabled: 0 },
    { scope: "user", skills: 2, mcp: 1, rules: 0, locks: 0, last_edited: "2026-11-06T10:00:00Z", disabled: 0 },
    { scope: "project", skills: 3, mcp: 1, rules: 1, locks: 0, last_edited: null, disabled: 1 },
  ],
  attention: [{ item_id: "itm_1", title: "Skill shadowed", subtitle: "Project copy wins", kind: "shadowed_skill", screen: "skills", row_id: "itm_1" }],
  attention_total: 1,
  changes: [{ id: "op-1", kind: "skill_added", summary: "Added skill", created_at: "2026-11-06T10:00:00Z", file_count: 2, scope: "project" }],
};

afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("OverviewScreen", () => {
  test("renders real scope summaries, proportional segment labels and clickable attention rows", async () => {
    ipc.reply = () => Promise.resolve(payload);
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Effective in this repo")).toBeTruthy();
    expect(screen.getByText("9 indexed items")).toBeTruthy();
    expect(screen.getByText("Global 11%")).toBeTruthy();
    expect(screen.getByText("User 33%")).toBeTruthy();
    expect(screen.getByText("Project 56%")).toBeTruthy();
    screen.getByRole("button", { name: /Skill shadowed/ }).click();
    expect(appState.getSnapshot().route).toBe("skills");
    expect(appState.getSnapshot().selections.skills).toEqual(["itm_1"]);
    expect(ipc.calls[0]?.command).toBe("overview_load");
  });
  test("shows retry after the database query fails", async () => {
    ipc.reply = () => Promise.reject(new Error("database unavailable"));
    render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
    expect(await screen.findByText("Overview could not load")).toBeTruthy();
    ipc.reply = () => Promise.resolve(payload);
    screen.getByRole("button", { name: "Retry" }).click();
    await waitFor(() => expect(screen.getByText("Effective in this repo")).toBeTruthy());
  });
});

test("renders loading skeletons and the scope empty state", async () => {
  ipc.reply = () => new Promise(() => {});
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findByText("Loading overview")).toBeTruthy();
  cleanup();
  ipc.reply = () => Promise.resolve({ scopes: [], attention: [], attention_total: 0, changes: [] });
  render(<AppStateProvider state={appState}><OverviewScreen /></AppStateProvider>);
  expect(await screen.findByText("No scope data")).toBeTruthy();
});
