import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { SecretInventoryScreen } from "./SecretInventoryScreen";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string): Promise<unknown> => Promise.resolve([]) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command); } }));
afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("SecretInventoryScreen", () => {
  test("shows empty state and creates only after matching masked confirmations", async () => {
    ipc.reply = (command) => Promise.resolve(command === "secrets_list" ? [] : undefined);
    render(<SecretInventoryScreen />);
    expect(await screen.findByText("No secrets yet")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Create secret" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "api-token" } });
    const value = screen.getByLabelText("Value") as HTMLInputElement;
    const confirmation = screen.getByLabelText("Confirm value") as HTMLInputElement;
    expect(value.type).toBe("password");
    expect(confirmation.type).toBe("password");
    fireEvent.change(value, { target: { value: "private-sentinel" } });
    fireEvent.change(confirmation, { target: { value: "different" } });
    expect((screen.getByRole("button", { name: "Save in Keychain" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(confirmation, { target: { value: "private-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save in Keychain" }));
    expect(await screen.findByText("No secrets yet")).toBeTruthy();
    expect(ipc.calls.find(({ command }) => command === "secrets_create")?.args).toEqual({ name: "api-token", value: "private-sentinel" });
  });

  test("lists no value column and removes an orphan only after a named confirmation", async () => {
    const orphan = { name: "old-token", account: "old-token", used_by_count: 0, used_by: [], created_at: null, rotated_at: null, last_read_ok_at: null, orphaned: true, available: true };
    ipc.reply = (command) => Promise.resolve(command === "secrets_list" ? [orphan] : undefined);
    render(<SecretInventoryScreen />);
    expect(await screen.findAllByText("old-token")).toHaveLength(2);
    expect(screen.getAllByRole("columnheader").map((cell) => cell.textContent)).not.toContain("Value");
    expect(screen.getByText("Orphaned")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = screen.getByRole("alertdialog");
    expect(dialog.textContent).toContain("Remove “old-token”?");
    expect(ipc.calls.some(({ command }) => command === "secrets_delete")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Remove secret" }));
    await waitFor(() => expect(ipc.calls.find(({ command }) => command === "secrets_delete")?.args).toEqual({ name: "old-token", confirm: true }));
  });
});
