import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** MCP row actions with explicit confirmation for destructive changes. */
export function McpServerActions({ name, onWhy, onEdit }: { readonly name: string; readonly onWhy: () => void; readonly onEdit: () => void }) {
  const [confirm, setConfirm] = useState<"disable" | "delete" | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  async function execute(action: "disable" | "delete") { try { await invoke(action === "disable" ? "mcp_disable_in_repo" : "mcp_delete", { name }); setMessage(action === "disable" ? "Server disabled" : "Server deleted"); } catch (cause) { setMessage(String(cause)); } finally { setConfirm(null); } }
  return <div><details><summary aria-label={`Actions for ${name}`}>⋯</summary><div role="menu"><button role="menuitem" onClick={onWhy}>Why is this on?</button><button role="menuitem" onClick={onEdit}>Edit</button><button role="menuitem" onClick={() => setConfirm("disable")}>Disable in this repo</button><button role="menuitem" onClick={() => setConfirm("delete")}>Delete server</button></div></details>
    {confirm && <section role="alertdialog" aria-modal="true"><h2>{confirm === "delete" ? "Delete server?" : "Disable server?"}</h2><p>{name} will {confirm === "delete" ? "be removed" : "stop loading in this repo"}.</p><button onClick={() => void execute(confirm)}>Confirm</button><button onClick={() => setConfirm(null)}>Cancel</button></section>}{message && <p role="status">{message}</p>}</div>;
}
