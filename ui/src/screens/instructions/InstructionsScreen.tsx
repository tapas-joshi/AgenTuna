import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";
import rehypeSanitize from "rehype-sanitize";
import { MarkdownEditor } from "../../components/editor/markdown-editor";
import { useAppSnapshot } from "../../state/context";

type Instruction = { item_id: string; scope: string; path: string; managed: boolean; content: string; hash: string; agents: string[] };

/** Scope-grouped rule browser and safe markdown editor. */
export function InstructionsScreen() {
  const { activeProject } = useAppSnapshot();
  const [files, setFiles] = useState<Instruction[]>([]);
  const [selected, setSelected] = useState<Instruction | null>(null);
  const [buffer, setBuffer] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState(false);
  const dirty = selected !== null && buffer !== selected.content;
  async function load() {
    setLoading(true); setError(null);
    try { const result = await invoke<Instruction[]>("instructions_list", { projectId: activeProject }); setFiles(result); setSelected((old) => result.find((f) => f.path === old?.path) ?? result[0] ?? null); }
    catch (cause) { setError(String(cause)); }
    finally { setLoading(false); }
  }
  useEffect(() => { void load(); }, [activeProject]);
  useEffect(() => { setBuffer(selected?.content ?? ""); }, [selected?.path, selected?.content]);
  async function save() {
    if (!selected || !dirty) return;
    setBusy(true); setError(null);
    try { await invoke("instruction_save", { request: { item_id: selected.item_id, path: selected.path, scope: selected.scope, content: buffer, expected_hash: selected.hash, project_id: activeProject } }); await load(); }
    catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  }
  async function createProjectRule() {
    if (!activeProject) { setError("Select a project before creating AGENTS.md."); return; }
    setBusy(true); setError(null);
    try { await invoke("instruction_create_project", { projectId: activeProject }); await load(); }
    catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  }
  useEffect(() => { const handler = (event: KeyboardEvent) => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") { event.preventDefault(); void save(); } }; window.addEventListener("keydown", handler); return () => window.removeEventListener("keydown", handler); });
  return <main aria-label="Instructions" className="mx-auto flex max-w-[1180px] flex-col gap-16 p-24">
    <header className="flex items-center justify-between"><div><h1 className="m-0 text-22 font-semibold text-text">Instructions</h1><p className="text-13 text-text-secondary">Rule files loaded by your agents, grouped by scope.</p></div><div className="flex gap-8"><button type="button" disabled={busy || !activeProject} onClick={() => void createProjectRule()}>New rule</button><button type="button" onClick={() => void load()}>Rescan</button></div></header>
    {loading ? <p role="status">Loading instruction files…</p> : error ? <div role="alert"><p>{error}</p><button onClick={() => void load()}>Retry</button></div> : files.length === 0 ? <section className="rounded-10 border border-border p-24"><h2>No instruction files found</h2><p>Create a rule file through the New rule action.</p><button disabled={busy || !activeProject} onClick={() => void createProjectRule()}>New rule</button></section> : <div className="grid min-h-[560px] grid-cols-[280px_1fr] gap-16">
      <aside aria-label="Instruction files" className="rounded-10 border border-border bg-raised p-12"><h2 className="text-14">Files</h2>{["global", "user", "project", "local"].map((scope) => <section key={scope}><h3 className="text-12 uppercase text-text-muted">{scope}</h3><ul className="m-0 list-none p-0">{files.filter((file) => file.scope.toLowerCase() === scope).map((file) => <li key={file.path}><button className="w-full truncate rounded-6 p-8 text-left" aria-pressed={selected?.path === file.path} onClick={() => { if (dirty && !window.confirm("Discard unsaved instruction changes?")) return; setSelected(file); }}>{file.path.split(/[\\/]/).at(-1)} <span className="text-10 text-text-muted">{file.managed ? "Managed" : "Native"}</span></button></li>)}</ul></section>)}</aside>
      <section className="min-w-0 rounded-10 border border-border bg-raised p-16"><div className="flex flex-wrap items-center justify-between gap-8 border-b border-border pb-12"><div className="min-w-0"><strong className="rounded-6 bg-scope-project/15 px-8 py-4 text-12">{selected?.scope}</strong><p className="truncate font-mono text-11 text-text-muted">{selected?.path}</p></div><div className="flex gap-8"><button aria-pressed={!preview} onClick={() => setPreview(false)}>Edit</button><button aria-pressed={preview} onClick={() => setPreview(true)}>Preview</button><span aria-label={dirty ? "Unsaved changes" : "Saved"}>{dirty ? "●" : "○"}</span><button disabled={!dirty || busy} onClick={() => { setBuffer(selected?.content ?? ""); }}>Discard</button><button disabled={!dirty || busy} onClick={() => void save()}>{busy ? "Saving…" : "Save"}</button></div></div>
        {selected ? preview ? <article className="prose max-w-none p-16"><ReactMarkdown rehypePlugins={[rehypeSanitize]}>{buffer}</ReactMarkdown></article> : <MarkdownEditor label="Instruction content" value={buffer} baseline={selected.content} onChange={setBuffer} onSave={() => void save()} className="min-h-[440px]" /> : <p>Select a rule file.</p>}
      </section></div>}
  </main>;
}
