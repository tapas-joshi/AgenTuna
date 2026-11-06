import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
import type { JSX } from "react";
import type { SecretInventoryView } from "../../generated/SecretInventoryView";

/** Keychain inventory screen; value text never enters a rendered element. */
export function SecretInventoryScreen(): JSX.Element {
  const [rows, setRows] = useState<readonly SecretInventoryView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<SecretInventoryView | null>(null);
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [confirmValue, setConfirmValue] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = async () => {
    setLoading(true);
    setError(null);
    try { setRows(await invoke<SecretInventoryView[]>("secrets_list")); }
    catch { setError("Secret inventory could not be loaded."); }
    finally { setLoading(false); }
  };
  useEffect(() => { void refresh(); }, []);

  const filtered = useMemo(() => rows.filter((row) => `${row.name} ${row.account} ${row.used_by.join(" ")}`.toLocaleLowerCase().includes(search.toLocaleLowerCase())), [rows, search]);
  const closeCreate = () => { setCreateOpen(false); setName(""); setValue(""); setConfirmValue(""); };
  const create = async () => {
    if (!name.trim() || value.length === 0 || value !== confirmValue) return;
    setBusy(true);
    try {
      await invoke("secrets_create", { name: name.trim(), value });
      closeCreate();
      await refresh();
    } catch { setError("Secret could not be saved to Keychain."); }
    finally { setBusy(false); setValue(""); setConfirmValue(""); }
  };
  const remove = async () => {
    if (confirmDelete === null) return;
    setBusy(true);
    try {
      await invoke("secrets_delete", { name: confirmDelete.name, confirm: true });
      setConfirmDelete(null);
      await refresh();
    } catch { setError(`Could not remove “${confirmDelete.name}”.`); }
    finally { setBusy(false); }
  };

  return <main className="mx-auto flex w-full max-w-[1040px] flex-col gap-20 p-24" aria-labelledby="secrets-heading">
    <header className="flex items-start justify-between gap-12"><div><p className="m-0 text-12 font-medium uppercase tracking-wide text-muted">Account / Keychain</p><h1 id="secrets-heading" className="mb-0 mt-4 text-24 font-semibold text-text">Keychain secrets</h1><p className="mb-0 mt-6 text-14 text-muted">Secret values stay in your operating system’s credential store. Servers add references when you save them.</p></div><button type="button" onClick={() => setCreateOpen(true)} className="rounded-8 bg-accent px-12 py-8 text-13 font-medium text-on-accent">Create secret</button></header>
    {error && <p role="alert" className="rounded-8 border border-danger/40 bg-danger/10 p-10 text-13 text-text">{error}</p>}
    <label className="flex max-w-[420px] flex-col gap-5 text-13 text-text">Search secrets<input type="search" value={search} onChange={(event) => setSearch(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-8" placeholder="Name, account or server" /></label>
    {loading ? <div role="status" className="rounded-10 border border-border p-20 text-13 text-muted">Loading secrets…</div> : rows.length === 0 ? <section className="rounded-12 border border-dashed border-border p-28 text-center"><h2 className="m-0 text-17 font-semibold text-text">No secrets yet</h2><p className="mb-0 mt-8 text-13 text-muted">Servers add secrets to Keychain when you save a server that references one.</p></section> : <div className="overflow-x-auto rounded-10 border border-border"><table className="w-full border-collapse text-left text-13"><thead className="bg-surface-muted text-12 text-muted"><tr>{["Name", "Account", "Used by", "Created", "Rotated", "Last read"].map((label) => <th key={label} className="px-12 py-10 font-medium">{label}</th>)}<th className="px-12 py-10" aria-label="Actions" /></tr></thead><tbody>{filtered.map((row) => <tr key={row.name} className="border-t border-border"><td className="px-12 py-12 font-medium text-text">{row.name}{!row.available && <span className="ml-8 text-danger">Missing</span>}</td><td className="px-12 py-12 font-mono text-12 text-muted">{row.account}</td><td className="px-12 py-12 text-text">{row.used_by.length ? row.used_by.join(", ") : <span className="text-muted">Orphaned</span>}</td><td className="px-12 py-12 text-muted">{row.created_at ?? "—"}</td><td className="px-12 py-12 text-muted">{row.rotated_at ?? "—"}</td><td className="px-12 py-12 text-muted">{row.last_read_ok_at ?? "—"}</td><td className="px-12 py-12"><button type="button" onClick={() => setConfirmDelete(row)} className="rounded-6 px-8 py-5 text-danger hover:bg-danger/10">Delete</button></td></tr>)}</tbody></table>{filtered.length === 0 && <p className="p-16 text-13 text-muted">No matching secrets.</p>}</div>}
    {createOpen && <div role="dialog" aria-modal="true" aria-labelledby="create-secret-heading" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="w-full max-w-[440px] rounded-12 border border-border bg-surface p-20"><h2 id="create-secret-heading" className="m-0 text-18 font-semibold text-text">Create a Keychain secret</h2><p className="text-13 text-muted">Only the name and account are stored in AgenTuna. The value is sent directly to Keychain.</p><div className="flex flex-col gap-12"><label className="flex flex-col gap-5 text-13 text-text">Name<input autoComplete="off" value={name} onChange={(event) => setName(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Value<input type="password" autoComplete="new-password" value={value} onChange={(event) => setValue(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Confirm value<input type="password" autoComplete="new-password" value={confirmValue} onChange={(event) => setConfirmValue(event.target.value)} className="rounded-8 border border-border bg-surface px-10 py-8" /></label>{value && confirmValue && value !== confirmValue && <p role="alert" className="m-0 text-12 text-danger">Values do not match.</p>}</div><div className="mt-18 flex justify-end gap-8"><button type="button" onClick={closeCreate} className="rounded-8 border border-border px-12 py-8 text-13">Cancel</button><button type="button" disabled={busy || !name.trim() || !value || value !== confirmValue} onClick={() => void create()} className="rounded-8 bg-accent px-12 py-8 text-13 font-medium text-on-accent disabled:opacity-50">Save in Keychain</button></div></section></div>}
    {confirmDelete && <div role="alertdialog" aria-modal="true" aria-labelledby="delete-secret-heading" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="w-full max-w-[420px] rounded-12 border border-border bg-surface p-20"><h2 id="delete-secret-heading" className="m-0 text-18 font-semibold text-text">Remove “{confirmDelete.name}”?</h2><p className="text-13 text-muted">This removes the Keychain value and reference. {confirmDelete.used_by_count > 0 ? `It is currently used by ${confirmDelete.used_by.join(", ")}.` : "This reference is orphaned."}</p><div className="flex justify-end gap-8"><button type="button" onClick={() => setConfirmDelete(null)} className="rounded-8 border border-border px-12 py-8 text-13">Cancel</button><button type="button" disabled={busy} onClick={() => void remove()} className="rounded-8 bg-danger px-12 py-8 text-13 text-white">Remove secret</button></div></section></div>}
  </main>;
}
