import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import type { SecretInventoryView } from "../../generated/SecretInventoryView";

/** Searchable value-free secret picker for an MCP environment row. */
export function SecretPicker({ onSelect, onClose }: {
  readonly onSelect: (reference: string) => void;
  readonly onClose: () => void;
}): JSX.Element {
  const [rows, setRows] = useState<readonly SecretInventoryView[]>([]);
  const [search, setSearch] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [createName, setCreateName] = useState("");
  const [value, setValue] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [rotation, setRotation] = useState<SecretInventoryView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = async () => {
    try { setRows(await invoke<SecretInventoryView[]>("secrets_list")); }
    catch { setError("Secret references could not be loaded."); }
  };
  useEffect(() => { void refresh(); }, []);
  const matches = useMemo(() => rows.filter((row) => row.name.toLocaleLowerCase().includes(search.toLocaleLowerCase())), [rows, search]);
  const clear = () => { setCreateName(""); setValue(""); setConfirmation(""); setRotation(null); };
  const close = () => { clear(); onClose(); };
  const save = async () => {
    const isNew = rotation === null;
    const name = rotation?.name ?? createName.trim();
    if (!name || !value || value !== confirmation) return;
    setBusy(true);
    setError(null);
    try {
      await invoke(isNew ? "secrets_create" : "secrets_rotate", { name, value });
      if (isNew) onSelect(`\u0024{${name}}`);
      clear();
      setCreateOpen(false);
      await refresh();
    } catch { setError("Keychain could not save this secret."); }
    finally { setBusy(false); setValue(""); setConfirmation(""); }
  };
  return <div role="dialog" aria-modal="true" aria-labelledby="secret-picker-heading" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="flex max-h-[80vh] w-full max-w-[560px] flex-col rounded-12 border border-border bg-surface p-18"><header className="flex items-start justify-between"><div><h2 id="secret-picker-heading" className="m-0 text-18 font-semibold text-text">Choose a Keychain secret</h2><p className="mb-0 mt-5 text-12 text-muted">Only reference names are shown. Secret values are never revealed.</p></div><button type="button" aria-label="Close secret picker" onClick={close} className="rounded-6 px-8 py-4 text-muted">×</button></header>
    {error && <p role="alert" className="rounded-8 bg-danger/10 p-8 text-12 text-danger">{error}</p>}
    <label className="mt-14 flex flex-col gap-5 text-12 text-text">Search<input type="search" autoFocus value={search} onChange={(event) => setSearch(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-8" placeholder="Secret name" /></label>
    <ul className="my-10 flex min-h-[90px] flex-col gap-4 overflow-y-auto p-0" aria-label="Matching secret references">{matches.map((row) => <li key={row.name} className="flex items-center justify-between gap-8 rounded-8 border border-border px-10 py-8"><button type="button" onClick={() => { onSelect(`\u0024{${row.name}}`); close(); }} className="min-w-0 flex-1 text-left"><span className="block truncate text-13 font-medium text-text">{row.name}</span><span className="block text-11 text-muted">{row.account} · used by {row.used_by_count}</span></button><button type="button" onClick={() => { setRotation(row); setCreateOpen(true); }} className="rounded-6 px-8 py-5 text-12 text-muted hover:bg-surface-muted">Rotate</button></li>)}</ul>
    {!matches.length && <p className="text-12 text-muted">No matching secret references.</p>}
    {createOpen ? <div className="mt-8 flex flex-col gap-8 border-t border-border pt-12"><h3 className="m-0 text-14 font-semibold text-text">{rotation ? `Rotate ${rotation.name}` : "Create new secret"}</h3>{!rotation && <label className="flex flex-col gap-4 text-12 text-text">Name<input value={createName} onChange={(event) => setCreateName(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-7" /></label>}<label className="flex flex-col gap-4 text-12 text-text">{rotation ? "New value" : "Value"}<input type="password" autoComplete="new-password" value={value} onChange={(event) => setValue(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-7" /></label><label className="flex flex-col gap-4 text-12 text-text">Confirm value<input type="password" autoComplete="new-password" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-7" /></label><div className="flex justify-end gap-8"><button type="button" onClick={() => { clear(); setCreateOpen(false); }} className="rounded-8 border border-border px-10 py-6 text-12">Cancel</button><button type="button" disabled={busy || !value || value !== confirmation || (!rotation && !createName.trim())} onClick={() => void save()} className="rounded-8 bg-accent px-10 py-6 text-12 text-on-accent disabled:opacity-50">Save</button></div></div> : <button type="button" onClick={() => { setRotation(null); setCreateOpen(true); }} className="mt-10 w-fit rounded-8 border border-border px-10 py-7 text-12">Create new secret</button>}
    </section></div>;
}
