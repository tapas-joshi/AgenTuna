import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { SecretPicker } from "./SecretPicker";
import { FixTokenDialog } from "./FixTokenDialog";

const ipc = vi.hoisted(() => ({ calls: [] as { command: string; args: unknown }[], reply: (_command: string): Promise<unknown> => Promise.resolve(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (command: string, args?: unknown) => { ipc.calls.push({ command, args }); return ipc.reply(command); } }));
const secret = { name: "github-token", account: "github-token", used_by_count: 1, used_by: ["github-server"], created_at: null, rotated_at: null, last_read_ok_at: null, orphaned: false, available: true };
afterEach(() => { cleanup(); ipc.calls.length = 0; });

describe("SecretPicker", () => {
  test("searches value-free references and assigns only the variable reference", async () => {
    const onSelect = vi.fn();
    ipc.reply = () => Promise.resolve([secret]);
    render(<SecretPicker onSelect={onSelect} onClose={() => undefined} />);
    expect(await screen.findByText("github-token")).toBeTruthy();
    expect(screen.queryByText(/private-value/)).toBeNull();
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "other" } });
    expect(screen.queryByText("github-token")).toBeNull();
    fireEvent.change(screen.getByRole("searchbox"), { target: { value: "github" } });
    fireEvent.click(await screen.findByText("github-token"));
    expect(onSelect).toHaveBeenCalledWith("${github-token}");
  });

  test("Fix token keeps Done disabled until testing succeeds and rotates only on Done", async () => {
    const saveServer = vi.fn(async () => undefined);
    ipc.reply = () => Promise.resolve(undefined);
    render(<FixTokenDialog secretName="github-token" testConnection={async () => ({ ok: true })} saveServer={saveServer} onClose={() => undefined} />);
    fireEvent.change(screen.getByLabelText("New token"), { target: { value: "sentinel-value" } });
    expect((screen.getByRole("button", { name: "Done" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByText(/test passed/i)).toBeTruthy();
    expect(saveServer).not.toHaveBeenCalled();
    expect(ipc.calls.some(({ command }) => command === "secrets_rotate")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(saveServer).toHaveBeenCalledTimes(1));
    expect(ipc.calls.find(({ command }) => command === "secrets_rotate")?.args).toEqual({ name: "github-token", value: "sentinel-value" });
  });
});
