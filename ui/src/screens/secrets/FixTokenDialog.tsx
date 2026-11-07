import { invoke } from "@tauri-apps/api/core";
import { useRef, useState } from "react";
import type { JSX } from "react";
import { SecretBuffer } from "./secret-buffer";

/** Focused token repair; server updates occur only after the injected test succeeds. */
export function FixTokenDialog({ secretName, testConnection, saveServer, onClose, buffer }: {
  readonly secretName: string;
  readonly testConnection: (value: string) => Promise<{ readonly ok: boolean }>;
  readonly saveServer: () => Promise<void>;
  readonly onClose: () => void;
  readonly buffer?: SecretBuffer;
}): JSX.Element {
  const bufferRef = useRef<SecretBuffer>(buffer ?? new SecretBuffer());
  const [value, setValue] = useState("");
  const [status, setStatus] = useState<"idle" | "testing" | "failed" | "passed" | "saving">("idle");
  const [message, setMessage] = useState<string | null>(null);
  const clearAndClose = () => { bufferRef.current.zeroize(); setValue(""); setMessage(null); onClose(); };
  const test = async () => {
    setStatus("testing"); setMessage(null);
    try {
      const result = await testConnection(value);
      setStatus(result.ok ? "passed" : "failed");
      if (!result.ok) setMessage("Connection test failed. Check the token and try again.");
    } catch { setStatus("failed"); setMessage("Connection test failed. Check the token and try again."); }
  };
  const done = async () => {
    if (status !== "passed") return;
    setStatus("saving");
    try {
      await invoke("secrets_rotate", { name: secretName, value });
      await saveServer();
      clearAndClose();
    } catch {
      setStatus("failed");
      setMessage("The token was tested, but the update could not be saved.");
    } finally { bufferRef.current.zeroize(); setValue(""); }
  };
  return <div role="dialog" aria-modal="true" aria-labelledby="fix-token-heading" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="w-full max-w-[440px] rounded-12 border border-border bg-surface p-20"><h2 id="fix-token-heading" className="m-0 text-18 font-semibold text-text">Fix token</h2><p className="text-13 text-muted">Enter a replacement for <strong>{secretName}</strong>. It stays in memory until this dialog closes.</p><label className="flex flex-col gap-5 text-13 text-text">New token<input type="password" autoComplete="new-password" value={value} onChange={(event) => { bufferRef.current.set(event.target.value); setValue(event.target.value); setStatus("idle"); setMessage(null); }} className="rounded-8 border border-border bg-surface px-10 py-8" /></label>{message && <p role="alert" className="mt-10 rounded-8 bg-danger/10 p-8 text-12 text-danger">{message}</p>}{status === "passed" && <p role="status" className="mt-10 text-12 text-success">Connection test passed. You can save the token.</p>}<div className="mt-16 flex justify-end gap-8"><button type="button" onClick={clearAndClose} className="rounded-8 border border-border px-12 py-8 text-13">Cancel</button><button type="button" disabled={!value || status === "testing" || status === "saving"} onClick={() => void test()} className="rounded-8 border border-border px-12 py-8 text-13">{status === "testing" ? "Testing…" : "Test connection"}</button><button type="button" disabled={status !== "passed"} onClick={() => void done()} className="rounded-8 bg-accent px-12 py-8 text-13 font-medium text-on-accent disabled:opacity-50">Done</button></div></section></div>;
}
