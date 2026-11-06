import { useState } from "react";
import type { FormEvent, JSX } from "react";
import { SecretEnvField } from "../secrets/SecretEnvField";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import { McpFilesThatWillChangePanel } from "../../components/mcp_form";

type Transport = "stdio" | "http";
type Scope = "global" | "user" | "project" | "local";
type Agent = "claude_code" | "codex" | "omp" | "cursor";
type EnvRow = { name: string; value: string; kind: "literal" | "secret" };

/** Draft submitted to the MCP planner; secret values are represented only as references. */
export interface AddMcpServerDraft {
  readonly name: string;
  readonly transport: Transport;
  readonly command: string;
  readonly args: readonly string[];
  readonly url: string;
  readonly headers: readonly { readonly name: string; readonly value: string }[];
  readonly env: readonly { readonly name: string; readonly value: string; readonly kind: "literal" | "secret" }[];
  readonly scope: Scope;
  readonly agents: readonly Agent[];
}

export interface AddMcpServerFormProps {
  readonly installedAgents?: readonly Agent[];
  readonly defaultAgents?: readonly Agent[];
  readonly globalAllowed?: boolean;
  readonly duplicateScope?: Scope | null;
  readonly onCancel: () => void;
  readonly onNameChange?: (name: string) => void;
  readonly onPlan: (draft: AddMcpServerDraft) => Promise<McpChangeSetPreviewView>;
  readonly onSave: (previewId: string, selectedAgents: readonly string[]) => Promise<void>;
}

/** Add-mode MCP form. Planning and filesystem changes are delegated to the app command. */
export function AddMcpServerForm({
  installedAgents = ["claude_code", "codex", "omp", "cursor"],
  defaultAgents = [],
  globalAllowed = false,
  duplicateScope = null,
  onCancel,
  onNameChange,
  onPlan,
  onSave,
}: AddMcpServerFormProps): JSX.Element {
  const [name, setName] = useState("");
  const [transport, setTransport] = useState<Transport>("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState<string[]>([""]);
  const [url, setUrl] = useState("");
  const [headers, setHeaders] = useState<Array<{ name: string; value: string }>>([{ name: "", value: "" }]);
  const [env, setEnv] = useState<EnvRow[]>([]);
  const [scope, setScope] = useState<Scope>("user");
  const [localExpanded, setLocalExpanded] = useState(false);
  const [agents, setAgents] = useState<Agent[]>(defaultAgents.filter((agent) => installedAgents.includes(agent)));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [preview, setPreview] = useState<McpChangeSetPreviewView | null>(null);

  const validate = () => {
    const next: Record<string, string> = {};
    if (!name.trim()) next.name = "Enter a server name.";
    if (transport === "stdio" && !command.trim()) next.command = "Enter a command.";
    if (transport === "http" && !/^https?:\/\//i.test(url.trim())) next.url = "Enter a valid HTTP or HTTPS URL.";
    if (!agents.length) next.agents = "Select at least one installed agent.";
    for (const [index, row] of env.entries()) {
      if (row.name && !/^[A-Za-z_][A-Za-z0-9_]*$/.test(row.name)) next[`env-${index}`] = "Use a valid environment variable name.";
    }
    setErrors(next);
    return Object.keys(next).length === 0 && duplicateScope === null;
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!validate() || saving) return;
    setSaving(true);
    setSaveError(null);
    try {
      setPreview(await onPlan({
        name: name.trim(), transport, command: command.trim(), args: args.filter(Boolean), url: url.trim(),
        headers: headers.filter((header) => header.name.trim()),
        env: env.filter((row) => row.name.trim()).map((row) => ({
          name: row.name.trim(), value: row.kind === "secret" ? `\u0024{${row.value}}` : row.value, kind: row.kind,
        })),
        scope, agents,
      }));
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : "The server could not be saved.");
    } finally {
      setSaving(false);
    }
  };
  const saveReviewedPlan = async () => {
    if (!preview || saving) return;
    setSaving(true);
    setSaveError(null);
    try {
      await onSave(preview.preview_id, [...new Set(preview.files.filter((file) => file.selected && file.agent !== "AgenTuna").map((file) => file.agent))]);
      setPreview(null);
      setName("");
      setCommand("");
      setArgs([""]);
      setUrl("");
      setHeaders([{ name: "", value: "" }]);
      setEnv([]);
      setAgents(defaultAgents.filter((agent) => installedAgents.includes(agent)));
      setErrors({});
      setSaved(true);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : "The server could not be saved.");
    } finally {
      setSaving(false);
    }
  };

  const toggleAgent = (agent: Agent, checked: boolean) => {
    setAgents((current) => checked
      ? (current.includes(agent) ? current : [...current, agent])
      : current.filter((entry) => entry !== agent));
  };

  return <main className="mx-auto grid w-full max-w-[960px] gap-20 p-24" aria-labelledby="mcp-add-title">
    <header className="flex items-start justify-between"><div><p className="m-0 text-12 uppercase tracking-wide text-muted">MCP Servers / Add</p><h1 id="mcp-add-title" className="mb-0 mt-4 text-24 font-semibold text-text">Add MCP server</h1><p className="mb-0 mt-6 text-13 text-muted">Configure the server once, then choose where and for whom it is available.</p></div><button type="button" onClick={onCancel} className="rounded-8 border border-border px-12 py-8 text-13">Cancel</button></header>
    {saved && <p role="status" className="rounded-8 border border-success/40 bg-success/10 p-10 text-13 text-text">MCP server saved.</p>}
    <form hidden={preview !== null} onSubmit={(event) => void submit(event)} noValidate className="grid gap-16">
      <label className="grid gap-5 text-13 text-text">Name<input aria-label="Name" value={name} onChange={(event) => { setName(event.target.value); onNameChange?.(event.target.value); setSaved(false); }} onBlur={validate} aria-invalid={Boolean(errors.name || duplicateScope)} aria-describedby={errors.name || duplicateScope ? "mcp-name-error" : undefined} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.name && <span id="mcp-name-error" role="alert" className="text-danger">{errors.name}</span>}{duplicateScope && <span id="mcp-name-error" role="alert" className="text-danger">A server with this name already exists in {duplicateScope} scope. Choose another name.</span>}</label>
      <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Transport</legend><div className="flex gap-8">{(["stdio", "http"] as const).map((kind) => <button key={kind} type="button" aria-pressed={transport === kind} onClick={() => setTransport(kind)} className={`rounded-8 border px-14 py-8 text-13 ${transport === kind ? "border-accent bg-accent/10" : "border-border"}`}>{kind === "stdio" ? "Command (stdio)" : "HTTP"}</button>)}</div></fieldset>
      {transport === "stdio" ? <><label className="grid gap-5 text-13 text-text">Command<input aria-label="Command" value={command} onChange={(event) => setCommand(event.target.value)} onBlur={validate} aria-invalid={Boolean(errors.command)} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.command && <span role="alert" className="text-danger">{errors.command}</span>}<span className="text-11 text-muted">Use an executable path or command resolved from PATH.</span></label><fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Arguments</legend>{args.map((arg, index) => <div className="flex gap-8" key={index}><input aria-label={`Argument ${index + 1}`} value={arg} onChange={(event) => setArgs(args.map((value, i) => i === index ? event.target.value : value))} className="min-w-0 flex-1 rounded-8 border border-border bg-surface px-10 py-8" /><button type="button" disabled={args.length === 1} aria-label={`Remove argument ${index + 1}`} onClick={() => setArgs(args.filter((_, i) => i !== index))}>Remove</button></div>)}<button type="button" onClick={() => setArgs([...args, ""])}>Add argument</button></fieldset></> : <><label className="grid gap-5 text-13 text-text">URL<input aria-label="URL" value={url} onChange={(event) => setUrl(event.target.value)} onBlur={validate} aria-invalid={Boolean(errors.url)} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.url && <span role="alert" className="text-danger">{errors.url}</span>}</label><fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Headers</legend>{headers.map((header, index) => <div className="grid grid-cols-2 gap-8" key={index}><input aria-label={`Header ${index + 1} name`} value={header.name} onChange={(event) => setHeaders(headers.map((row, i) => i === index ? { ...row, name: event.target.value } : row))} placeholder="Header name" className="rounded-8 border border-border bg-surface px-10 py-8" /><input aria-label={`Header ${index + 1} value`} value={header.value} onChange={(event) => setHeaders(headers.map((row, i) => i === index ? { ...row, value: event.target.value } : row))} placeholder="Value" className="rounded-8 border border-border bg-surface px-10 py-8" /></div>)}<button type="button" onClick={() => setHeaders([...headers, { name: "", value: "" }])}>Add header</button></fieldset><section aria-label="Sign in" className="rounded-8 border border-border p-12"><h2 className="m-0 text-13 font-medium">Sign in</h2><p className="mb-0 mt-5 text-12 text-muted">Authentication setup is available after the server is saved.</p></section></>}
      <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Environment variables</legend><p className="m-0 text-11 text-muted">Files get ${"${NAME}"}; the value itself never lands on disk.</p>{env.map((row, index) => <div key={index} className="grid gap-6 rounded-8 border border-border p-8"><div className="flex gap-8"><input aria-label={`Variable ${index + 1} name`} value={row.name} onChange={(event) => setEnv(env.map((value, i) => i === index ? { ...value, name: event.target.value } : value))} placeholder="KEY" className="w-1/3 rounded-8 border border-border bg-surface px-10 py-8" /><select aria-label={`Variable ${index + 1} value type`} value={row.kind} onChange={(event) => setEnv(env.map((value, i) => i === index ? { ...value, kind: event.target.value as EnvRow["kind"], value: "" } : value))} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="secret">Keychain reference</option><option value="literal">Literal value</option></select><button type="button" onClick={() => setEnv(env.filter((_, i) => i !== index))}>Remove</button></div>{row.kind === "secret" ? <SecretEnvField name={row.name || "KEY"} reference={row.value ? `\u0024{${row.value}}` : ""} onChange={(value) => setEnv(env.map((entry, i) => i === index ? { ...entry, value: value.replace(/^\u0024\{|\}$/g, "") } : entry))} /> : <input aria-label={`Variable ${index + 1} literal value`} value={row.value} onChange={(event) => setEnv(env.map((entry, i) => i === index ? { ...entry, value: event.target.value } : entry))} placeholder="Value" className="rounded-8 border border-border bg-surface px-10 py-8" />}{errors[`env-${index}`] && <span role="alert" className="text-danger">{errors[`env-${index}`]}</span>}</div>)}<button type="button" onClick={() => setEnv([...env, { name: "", value: "", kind: "secret" }])}>Add variable</button></fieldset>
      <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Scope</legend><div className="grid grid-cols-1 gap-8 md:grid-cols-3">{(["global", "user", "project"] as const).map((value) => <label key={value} title={value === "global" && !globalAllowed ? "Administrator permission is unavailable" : undefined} className={`rounded-8 border p-12 ${value === "global" && !globalAllowed ? "opacity-50" : "border-border"}`}><input aria-label={value[0]?.toUpperCase() + value.slice(1)} type="radio" name="scope" value={value} disabled={value === "global" && !globalAllowed} checked={scope === value} onChange={() => setScope(value)} /> <span className="ml-6 text-13">{value[0]?.toUpperCase()}{value.slice(1)}</span><span className="mt-4 block text-11 text-muted">{value === "global" ? "Needs admin" : value === "user" ? "Best for personal tokens" : "Committed"}</span></label>)}</div><details open={localExpanded} onToggle={(event) => setLocalExpanded(event.currentTarget.open)} className="rounded-8 border border-border p-10"><summary className="cursor-pointer text-13">Local (only you, this repo)</summary>{localExpanded && <label className="mt-8 flex items-center gap-8 text-13"><input type="radio" aria-label="Local" name="scope" checked={scope === "local"} onChange={() => setScope("local")} />Save into .agentuna/local/</label>}</details></fieldset>
      <fieldset className="grid gap-8">
        <legend className="text-13 font-medium text-text">Agents</legend>
        {(["claude_code", "codex", "omp", "cursor"] as const).map((agent) => {
          const label = agent === "claude_code" ? "Claude Code" : agent === "codex" ? "Codex" : agent;
          const selected = agents.includes(agent);
          return <div key={agent} className="flex items-center gap-8">
            <button type="button" aria-pressed={selected} disabled={!installedAgents.includes(agent)} onClick={() => toggleAgent(agent, !selected)} className={`rounded-8 border px-10 py-6 text-13 ${selected ? "border-accent bg-accent/10" : "border-border"}`}>
              {label}
            </button>
            {!installedAgents.includes(agent) && <span className="text-11 text-muted">not installed</span>}
          </div>;
        })}
        {errors.agents && <span role="alert" className="text-danger">{errors.agents}</span>}
      </fieldset>
      {saveError && <p role="alert" className="text-13 text-danger">Save failed: {saveError}</p>}
      <footer className="flex justify-end gap-8 border-t border-border pt-12"><button type="button" onClick={onCancel} className="rounded-8 border border-border px-14 py-8 text-13">Cancel</button><button type="submit" disabled={saving || duplicateScope !== null || preview !== null} className="rounded-8 bg-accent px-14 py-8 text-13 text-on-accent disabled:opacity-50">{saving ? "Planning…" : "Review changes"}</button></footer>
    </form>
    {preview && <McpFilesThatWillChangePanel
      preview={preview}
      onAgentSelectedChange={(agent, selected) => setPreview((current) => current === null ? current : {
        ...current,
        files: current.files.map((file) => file.agent === agent ? { ...file, selected } : file),
      })}
      onSave={() => void saveReviewedPlan()}
      onCancel={() => setPreview(null)}
    />}
  </main>;
}
