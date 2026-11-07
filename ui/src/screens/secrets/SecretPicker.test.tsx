import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { SecretPicker } from "./SecretPicker";
import { FixTokenDialog } from "./FixTokenDialog";
import { SecretBuffer } from "./secret-buffer";

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

  test("keeps Done disabled after a failed token test and rotates nothing", async () => {
    const saveServer = vi.fn(async () => undefined);
    ipc.reply = () => Promise.resolve(undefined);
    render(<FixTokenDialog secretName="github-token" testConnection={async () => ({ ok: false })} saveServer={saveServer} onClose={() => undefined} />);
    fireEvent.change(screen.getByLabelText("New token"), { target: { value: "wrong-value" } });
    fireEvent.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByText(/Connection test failed/)).toBeTruthy();
    expect((screen.getByRole("button", { name: "Done" }) as HTMLButtonElement).disabled).toBe(true);
    expect(saveServer).not.toHaveBeenCalled();
    expect(ipc.calls.some(({ command }) => command === "secrets_rotate")).toBe(false);
  });

  test("creates a secret, marks it used by this form and never renders its value", async () => {
    const onSelect = vi.fn();
    ipc.reply = (command) => Promise.resolve(command === "secrets_list" ? [] : undefined);
    render(<SecretPicker onSelect={onSelect} onClose={() => undefined} />);
    fireEvent.click(await screen.findByRole("button", { name: "Create new secret" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "api-token" } });
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "private-sentinel" } });
    fireEvent.change(screen.getByLabelText("Confirm value"), { target: { value: "private-sentinel" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(ipc.calls.find(({ command }) => command === "secrets_create")?.args).toEqual({ name: "api-token", value: "private-sentinel" }));
    expect(onSelect).toHaveBeenCalledWith("${api-token}");
    expect(await screen.findByText("Used by this form: ${api-token}")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /reveal/i })).toBeNull();
    expect(screen.queryByDisplayValue("private-sentinel")).toBeNull();
  });

  test("closing the picker and the token dialog releases their buffers", async () => {
    const pickerBuffer = new SecretBuffer();
    const onClose = vi.fn();
    ipc.reply = () => Promise.resolve([]);
    const { unmount } = render(<SecretPicker buffer={pickerBuffer} onSelect={() => undefined} onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Create new secret" }));
    fireEvent.change(screen.getByLabelText("Value"), { target: { value: "typed-secret" } });
    expect(pickerBuffer.value).toBe("typed-secret");
    fireEvent.click(screen.getByRole("button", { name: "Close secret picker" }));
    expect(pickerBuffer.zeroized).toBe(true);
    expect(pickerBuffer.value).toBe("");
    expect(onClose).toHaveBeenCalledOnce();
    unmount();

    const dialogBuffer = new SecretBuffer();
    const dialogClose = vi.fn();
    render(<FixTokenDialog buffer={dialogBuffer} secretName="github-token" testConnection={async () => ({ ok: true })} saveServer={async () => undefined} onClose={dialogClose} />);
    fireEvent.change(screen.getByLabelText("New token"), { target: { value: "typed-token" } });
    expect(dialogBuffer.value).toBe("typed-token");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(dialogBuffer.zeroized).toBe(true);
    expect(dialogBuffer.value).toBe("");
    expect(dialogClose).toHaveBeenCalledOnce();
  });
});
