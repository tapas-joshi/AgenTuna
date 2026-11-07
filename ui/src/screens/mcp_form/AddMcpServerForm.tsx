import { useState } from "react";
import type { FormEvent, JSX } from "react";
import { SecretEnvField } from "../secrets/SecretEnvField";
import type { McpChangeSetPreviewView } from "../../generated/McpChangeSetPreviewView";
import type { McpLaunchApproval } from "../../generated/McpLaunchApproval";
import type { McpLaunchApprovalView } from "../../generated/McpLaunchApprovalView";
import type { McpTestResultView } from "../../generated/McpTestResultView";
import { McpFilesThatWillChangePanel, McpLaunchConfirmDialog, McpTestConnectionPanel } from "../../components/mcp_form";

type Transport = "stdio" | "http";
type Scope = "global" | "user" | "project" | "local";
type Agent = "claude_code" | "codex" | "omp" | "cursor";
type EnvRow = { name: string; value: string; kind: "literal" | "secret" };

/** Existing server values used to prefill the shared add/edit form. */
export interface McpServerFormInitialState extends AddMcpServerDraft {
  readonly id: string;
}

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
  readonly mode?: "add" | "edit";
  readonly initialState?: McpServerFormInitialState;
  readonly initialScope?: Scope;
  readonly installedAgents?: readonly Agent[];
  readonly defaultAgents?: readonly Agent[];
  readonly globalAllowed?: boolean;
  readonly duplicateScope?: Scope | null;
  readonly onCancel: () => void;
  readonly onNameChange?: (name: string) => void;
  readonly onPlan: (draft: AddMcpServerDraft, itemId?: string) => Promise<McpChangeSetPreviewView>;
  readonly onSave: (previewId: string, selectedAgents: readonly string[], acknowledgeUnsupported: boolean) => Promise<void>;
  readonly onPrepareLaunch?: (draft: AddMcpServerDraft) => Promise<McpLaunchApprovalView>;
  readonly onTest?: (draft: AddMcpServerDraft, approval: McpLaunchApproval) => Promise<McpTestResultView>;
  readonly onSuggestedAction?: (action: string) => void;
}

/** Whether a rejected save is the typed external-write conflict the dialog answers. */
function isWriteConflict(error: unknown): error is { readonly code: "external_write_conflict" } {
  return typeof error === "object" && error !== null && "code" in error && error.code === "external_write_conflict";
}

/** Shared add/edit MCP form. Planning and filesystem changes are delegated to the app command. */
export function AddMcpServerForm({
  mode = "add",
  initialState,
  initialScope = "user",
  installedAgents = ["claude_code", "codex", "omp", "cursor"],
  defaultAgents = [],
  globalAllowed = false,
  duplicateScope = null,
  onCancel,
  onNameChange,
  onPlan,
  onSave,
  onPrepareLaunch,
  onTest,
  onSuggestedAction,
}: AddMcpServerFormProps): JSX.Element {
  const [name, setName] = useState(initialState?.name ?? "");
  const [transport, setTransport] = useState<Transport>(initialState?.transport ?? "stdio");
  const [command, setCommand] = useState(initialState?.command ?? "");
  const [args, setArgs] = useState<string[]>(initialState ? [...initialState.args] : [""]);
  const [url, setUrl] = useState(initialState?.url ?? "");
  const [headers, setHeaders] = useState<Array<{ name: string; value: string }>>(initialState ? initialState.headers.map((header) => ({ ...header })) : [{ name: "", value: "" }]);
  const [env, setEnv] = useState<EnvRow[]>(initialState ? initialState.env.map((row) => ({ ...row })) : []);
  const [scope, setScope] = useState<Scope>(initialState?.scope ?? initialScope);
  const [localExpanded, setLocalExpanded] = useState(scope === "local");
  const [agents, setAgents] = useState<Agent[]>([...(initialState?.agents ?? defaultAgents)].filter((agent) => installedAgents.includes(agent)));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [preview, setPreview] = useState<McpChangeSetPreviewView | null>(null);
  const [discardPrompt, setDiscardPrompt] = useState(false);
  const [testResult, setTestResult] = useState<McpTestResultView | null>(null);
  const [testedSignature, setTestedSignature] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [pendingApproval, setPendingApproval] = useState<McpLaunchApprovalView | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [renameConfirm, setRenameConfirm] = useState<McpChangeSetPreviewView | null>(null);
  const [conflict, setConflict] = useState(false);
  const current = JSON.stringify({
    name, transport, command, args: args.filter(Boolean), url,
    headers: headers.filter((header) => header.name.trim()),
    env: env.filter((row) => row.name.trim()), scope, agents,
  });
  const initial = JSON.stringify({
    name: initialState?.name ?? "", transport: initialState?.transport ?? "stdio",
    command: initialState?.command ?? "", args: (initialState?.args ?? []).filter(Boolean),
    url: initialState?.url ?? "", headers: (initialState?.headers ?? []).filter((header) => header.name.trim()),
    env: (initialState?.env ?? []).filter((row) => row.name.trim()),
    scope: initialState?.scope ?? initialScope,
    agents: (initialState?.agents ?? defaultAgents).filter((agent) => installedAgents.includes(agent)),
  });
  const dirty = mode === "add" || current !== initial;
  const signature = current;
  const testStale = testResult !== null && testedSignature !== signature;
  const renamed = mode === "edit" && name.trim() !== (initialState?.name ?? "");
  const canSave = preview === null || (preview.files.length > 0 && preview.files.every((file) => !file.selected || file.cannot_write_reason === null) && ((preview.unsupported_fields ?? []).length === 0 || acknowledged));
  const buildDraft = (): AddMcpServerDraft => ({
    name: name.trim(),
    transport,
    command: command.trim(),
    args: args.filter(Boolean),
    url: url.trim(),
    headers: headers.filter((header) => header.name.trim()),
    env: env.filter((row) => row.name.trim()).map((row) => ({
      name: row.name.trim(),
      value: row.kind === "secret" ? `\u0024{${row.value}}` : row.value,
      kind: row.kind,
    })),
    scope,
    agents,
  });
  const initialDraft = (): AddMcpServerDraft => ({
    name: initialState?.name ?? "",
    transport: initialState?.transport ?? "stdio",
    command: initialState?.command ?? "",
    args: [...(initialState?.args ?? [])],
    url: initialState?.url ?? "",
    headers: (initialState?.headers ?? []).map((header) => ({ ...header })),
    env: (initialState?.env ?? []).map((row) => ({ ...row })),
    scope: initialState?.scope ?? "user",
    agents: (initialState?.agents ?? defaultAgents).filter((agent) => installedAgents.includes(agent)),
  });
  const restoreInitial = () => {
    setName(initialState?.name ?? "");
    setTransport(initialState?.transport ?? "stdio");
    setCommand(initialState?.command ?? "");
    setArgs(initialState ? [...initialState.args] : [""]);
    setUrl(initialState?.url ?? "");
    setHeaders(initialState ? initialState.headers.map((header) => ({ ...header })) : [{ name: "", value: "" }]);
    setEnv(initialState ? initialState.env.map((row) => ({ ...row })) : []);
  };
  const closeForm = () => {
    if (mode === "edit" && dirty) setDiscardPrompt(true);
    else onCancel();
  };
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
  const failedTestResult = (error: unknown): McpTestResultView => ({
    ok: false,
    server_name: name.trim(),
    server_version: null,
    transport,
    latency_ms: 0n,
    tools_count: 0n,
    tools: [],
    error: {
      kind: "test_failed",
      message: error instanceof Error ? error.message : "The connection test could not run.",
      suggested_action: "Try again",
    },
    stderr_excerpt: null,
  });
  const requestLaunch = async () => {
    if (!onPrepareLaunch || preparing || testing) return;
    setPreparing(true);
    setSaveError(null);
    try {
      setPendingApproval(await onPrepareLaunch(buildDraft()));
    } catch (error) {
      setTestResult(failedTestResult(error));
    } finally {
      setPreparing(false);
    }
  };
  const confirmLaunch = async () => {
    if (!onTest || pendingApproval === null || testing) return;
    const approval: McpLaunchApproval = {
      approval_id: pendingApproval.approval_id,
      request_hash: pendingApproval.request_hash,
    };
    const snapshot = signature;
    setPendingApproval(null);
    setTesting(true);
    setSaveError(null);
    try {
      setTestResult(await onTest(buildDraft(), approval));
    } catch (error) {
      setTestResult(failedTestResult(error));
    } finally {
      setTestedSignature(snapshot);
      setTesting(false);
    }
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!validate() || saving || (mode === "edit" && !dirty)) return;
    if (preview !== null) {
      requestApply(preview);
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      setPreview(await onPlan(buildDraft(), mode === "edit" ? initialState?.id : undefined));
      setAcknowledged(false);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : "The server could not be saved.");
    } finally {
      setSaving(false);
    }
  };
  const applyReviewedPlan = async (value: McpChangeSetPreviewView) => {
    if (saving) return;
    setSaving(true);
    setSaveError(null);
    try {
      const targets = [...new Set(value.files.filter((file) => file.selected && file.agent !== "AgenTuna").map((file) => file.agent))];
      await onSave(value.preview_id, targets, acknowledged);
      setPreview(null);
      setRenameConfirm(null);
      setConflict(false);
      setAcknowledged(false);
      if (mode === "add") {
        setName("");
        setCommand("");
        setArgs([""]);
        setUrl("");
        setHeaders([{ name: "", value: "" }]);
        setEnv([]);
        setAgents(defaultAgents.filter((agent) => installedAgents.includes(agent)));
        setScope(initialScope);
        setErrors({});
        setSaved(true);
      } else {
        onCancel();
      }
    } catch (error) {
      if (isWriteConflict(error)) setConflict(true);
      else setSaveError(error instanceof Error ? error.message : "The server could not be saved.");
    } finally {
      setSaving(false);
    }
  };
  const requestApply = (value: McpChangeSetPreviewView) => {
    if (renamed) setRenameConfirm(value);
    else void applyReviewedPlan(value);
  };
  const replan = async (keepMine: boolean) => {
    setConflict(false);
    setSaving(true);
    setSaveError(null);
    const draft = keepMine ? buildDraft() : initialDraft();
    if (!keepMine) restoreInitial();
    try {
      setPreview(await onPlan(draft, mode === "edit" ? initialState?.id : undefined));
      setAcknowledged(false);
    } catch (error) {
      setPreview(null);
      setSaveError(error instanceof Error ? error.message : "The server could not be planned.");
    } finally {
      setSaving(false);
    }
  };
  const toggleAgent = (agent: Agent, checked: boolean) => {
    setAgents((currentAgents) => checked
      ? (currentAgents.includes(agent) ? currentAgents : [...currentAgents, agent])
      : currentAgents.filter((entry) => entry !== agent));
  };


  return <main className="mx-auto flex w-full max-w-[1280px] flex-col gap-24 p-24" aria-labelledby="mcp-add-title">
    <header className="flex flex-col gap-4">
      <p className="m-0 text-12 uppercase tracking-wide text-muted">MCP Servers / {mode === "edit" ? (initialState?.name ?? name) : "New server"}</p>
      <h1 id="mcp-add-title" className="mb-0 mt-4 text-26 font-semibold text-text">{mode === "edit" ? `Edit ${initialState?.name ?? name}` : "Add MCP server"}</h1>
      <p className="mb-0 mt-6 text-13 text-muted">{mode === "edit" ? "Changes go to every selected agent's config file. AgenTuna backs up each file first." : "Configure once. AgenTuna writes it into every selected agent's own config file."}</p>
    </header>
    {saved && <p role="status" className="rounded-8 border border-success/40 bg-success/10 p-10 text-13 text-text">MCP server saved.</p>}
    <form onSubmit={(event) => void submit(event)} noValidate className="flex flex-col gap-24">
      <div className="grid grid-cols-1 items-start gap-24 lg:grid-cols-[1.1fr_0.9fr]">
        <div className="flex flex-col gap-24">
          <label className="grid gap-5 text-13 text-text">Name<input aria-label="Name" value={name} onChange={(event) => { setName(event.target.value); onNameChange?.(event.target.value); setSaved(false); }} aria-invalid={Boolean(errors.name || duplicateScope)} aria-describedby={errors.name || duplicateScope ? "mcp-name-error" : undefined} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.name && <span id="mcp-name-error" role="alert" className="text-danger">{errors.name}</span>}{duplicateScope && <span id="mcp-name-error" role="alert" className="text-danger">A server with this name already exists in {duplicateScope} scope. Choose another name.</span>}</label>
          <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Transport</legend><div className="flex gap-8">{(["stdio", "http"] as const).map((kind) => <button key={kind} type="button" aria-pressed={transport === kind} onClick={() => setTransport(kind)} className={`rounded-8 border px-14 py-8 text-13 ${transport === kind ? "border-accent bg-accent/10" : "border-border"}`}>{kind === "stdio" ? "Command (stdio)" : "HTTP"}</button>)}</div></fieldset>
          {transport === "stdio" ? <><label className="grid gap-5 text-13 text-text">Command<input aria-label="Command" value={command} onChange={(event) => setCommand(event.target.value)} aria-invalid={Boolean(errors.command)} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.command && <span role="alert" className="text-danger">{errors.command}</span>}<span className="text-11 text-muted">Use an executable path or command resolved from PATH.</span></label><fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Arguments</legend>{args.map((arg, index) => <div className="flex gap-8" key={index}><input aria-label={`Argument ${index + 1}`} value={arg} onChange={(event) => setArgs(args.map((value, i) => i === index ? event.target.value : value))} className="min-w-0 flex-1 rounded-8 border border-border bg-surface px-10 py-8" /><button type="button" disabled={args.length === 1} aria-label={`Remove argument ${index + 1}`} onClick={() => setArgs(args.filter((_, i) => i !== index))}>Remove</button></div>)}<button type="button" onClick={() => setArgs([...args, ""])}>Add argument</button></fieldset></> : <><label className="grid gap-5 text-13 text-text">URL<input aria-label="URL" value={url} onChange={(event) => setUrl(event.target.value)} aria-invalid={Boolean(errors.url)} className="rounded-8 border border-border bg-surface px-10 py-8" />{errors.url && <span role="alert" className="text-danger">{errors.url}</span>}</label><fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Headers</legend>{headers.map((header, index) => <div className="grid grid-cols-2 gap-8" key={index}><input aria-label={`Header ${index + 1} name`} value={header.name} onChange={(event) => setHeaders(headers.map((row, i) => i === index ? { ...row, name: event.target.value } : row))} placeholder="Header name" className="rounded-8 border border-border bg-surface px-10 py-8" /><input aria-label={`Header ${index + 1} value`} value={header.value} onChange={(event) => setHeaders(headers.map((row, i) => i === index ? { ...row, value: event.target.value } : row))} placeholder="Value" className="rounded-8 border border-border bg-surface px-10 py-8" /></div>)}<button type="button" onClick={() => setHeaders([...headers, { name: "", value: "" }])}>Add header</button></fieldset><section aria-label="Sign in" className="rounded-8 border border-border p-12"><h2 className="m-0 text-13 font-medium">Sign in</h2><p className="mb-0 mt-5 text-12 text-muted">Authentication setup is available after the server is saved.</p></section></>}
          <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Environment variables</legend><p className="m-0 text-11 text-muted">Files get {"${NAME}"}; the value itself never lands on disk.</p>{env.map((row, index) => <div key={index} className="grid gap-6 rounded-8 border border-border p-8"><div className="flex gap-8"><input aria-label={`Variable ${index + 1} name`} value={row.name} onChange={(event) => setEnv(env.map((value, i) => i === index ? { ...value, name: event.target.value } : value))} placeholder="KEY" className="w-1/3 rounded-8 border border-border bg-surface px-10 py-8" /><select aria-label={`Variable ${index + 1} value type`} value={row.kind} onChange={(event) => setEnv(env.map((value, i) => i === index ? { ...value, kind: event.target.value as EnvRow["kind"], value: "" } : value))} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="secret">Keychain reference</option><option value="literal">Literal value</option></select><button type="button" onClick={() => setEnv(env.filter((_, i) => i !== index))}>Remove</button></div>{row.kind === "secret" ? <SecretEnvField name={row.name || "KEY"} reference={row.value ? `\u0024{${row.value}}` : ""} onChange={(value) => setEnv(env.map((entry, i) => i === index ? { ...entry, value: value.replace(/^\u0024\{|\}$/g, "") } : entry))} /> : <input aria-label={`Variable ${index + 1} literal value`} value={row.value} onChange={(event) => setEnv(env.map((entry, i) => i === index ? { ...entry, value: event.target.value } : entry))} placeholder="Value" className="rounded-8 border border-border bg-surface px-10 py-8" />}{errors[`env-${index}`] && <span role="alert" className="text-danger">{errors[`env-${index}`]}</span>}</div>)}<button type="button" onClick={() => setEnv([...env, { name: "", value: "", kind: "secret" }])}>Add variable</button></fieldset>
          <fieldset className="grid gap-8"><div className="flex items-baseline gap-8"><legend className="text-13 font-medium text-text">Scope</legend><span className="text-11 text-muted">where the config is written</span></div><div className="grid grid-cols-1 gap-12 sm:grid-cols-3">{([ { id: "global" as Scope, title: "Global", desc: "Every user on this Mac. Needs admin.", disabled: !globalAllowed, badge: undefined }, { id: "user" as Scope, title: "User", desc: "You, in every repo.", disabled: false, badge: "Best for personal tokens" }, { id: "project" as Scope, title: "Project", desc: "This repo, committed to git. Teammates get it too.", disabled: false, badge: undefined } ]).map((item) => { const checked = scope === item.id; return <label key={item.id} title={item.id === "global" && !globalAllowed ? "Administrator permission is unavailable" : undefined} className={`flex flex-col justify-between gap-8 rounded-10 border p-12 cursor-pointer transition-colors ${item.disabled ? "opacity-50 cursor-not-allowed border-border" : checked ? "border-accent bg-accent/10" : "border-border bg-raised hover:border-text-muted"}`}><div className="flex items-start gap-8"><input aria-label={item.title} type="radio" name="scope" value={item.id} disabled={item.disabled} checked={checked} onChange={() => { setScope(item.id); setSaved(false); }} className="mt-2 text-accent" /><div className="flex flex-col gap-2"><span className="text-13 font-semibold text-text">{item.title}</span><span className="text-11 text-muted">{item.desc}</span></div></div>{item.badge && <span className="mt-4 self-start rounded-4 bg-accent/15 px-6 py-2 text-11 font-medium text-accent">{item.badge}</span>}</label>; })}</div><details open={localExpanded} onToggle={(event) => setLocalExpanded(event.currentTarget.open)} className="rounded-8 border border-border p-10"><summary className="cursor-pointer text-13">Local (only you, this repo)</summary>{localExpanded && <label className="mt-8 flex items-center gap-8 text-13"><input type="radio" aria-label="Local" name="scope" checked={scope === "local"} onChange={() => setScope("local")} />Save into .agentuna/local/</label>}</details></fieldset>
          <fieldset className="grid gap-8"><legend className="text-13 font-medium text-text">Agents</legend><div className="flex flex-wrap gap-8">{(["claude_code", "codex", "omp", "cursor"] as const).map((agent) => { const label = agent === "claude_code" ? "Claude Code" : agent === "codex" ? "Codex" : agent; const selected = agents.includes(agent); return <div key={agent} className="flex items-center gap-8"><button type="button" aria-pressed={selected} disabled={!installedAgents.includes(agent)} onClick={() => toggleAgent(agent, !selected)} className={`rounded-8 border px-10 py-6 text-13 ${selected ? "border-accent bg-accent/10" : "border-border"}`}>{label}</button>{!installedAgents.includes(agent) && <span className="text-11 text-muted">not installed</span>}</div>; })}</div>{errors.agents && <span role="alert" className="text-danger">{errors.agents}</span>}</fieldset>
          {saveError && <p role="alert" className="text-13 text-danger">Save failed: {saveError}</p>}
        </div>
        <div className="flex flex-col gap-24">
          {onTest && onPrepareLaunch && <McpTestConnectionPanel
            result={testStale ? null : testResult}
            running={testing || preparing}
            stale={testStale}
            onRunTest={() => void requestLaunch()}
            onSuggestedAction={onSuggestedAction}
          />}
          {preview ? <McpFilesThatWillChangePanel preview={preview} onAgentSelectedChange={(agent, selected) => setPreview((value) => value === null ? value : { ...value, files: value.files.map((file) => file.agent === agent ? { ...file, selected } : file) })} acknowledged={acknowledged} onAcknowledgedChange={setAcknowledged} /> : <section aria-labelledby="mcp-files-heading" className="grid gap-8 rounded-10 border border-border bg-surface p-16" data-slot="mcp-files-panel"><div className="flex items-center justify-between gap-8"><h2 id="mcp-files-heading" className="text-13 font-semibold text-text">Files that will change</h2><span className="text-11 text-muted">0 files · backup first</span></div><p className="m-0 text-11 text-muted">Configure server name and command to preview what files will change.</p></section>}
        </div>
      </div>
      <footer className="sticky bottom-0 z-20 -mx-24 -mb-24 flex items-center justify-end gap-12 border-t border-border bg-surface/95 backdrop-blur px-24 py-16">
        <button type="button" onClick={closeForm} className="rounded-8 border border-border px-14 py-8 text-13">Cancel</button>
        <button type="submit" disabled={saving || duplicateScope !== null || (mode === "edit" && !dirty) || !canSave} className="rounded-8 bg-accent-fill px-14 py-8 text-13 text-on-accent disabled:opacity-50">{saving ? "Planning…" : (preview ? `Save to ${scope.charAt(0).toUpperCase() + scope.slice(1)} scope` : (mode === "edit" ? "Review edit" : "Review changes"))}</button>
      </footer>
    </form>
    {discardPrompt && <div role="dialog" aria-modal="true" aria-labelledby="mcp-discard-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="grid max-w-[420px] gap-12 rounded-12 border border-border bg-surface p-20"><h2 id="mcp-discard-title" className="m-0 text-18">Discard unsaved changes?</h2><p className="m-0 text-13 text-muted">Your MCP server edits have not been applied.</p><div className="flex justify-end gap-8"><button type="button" onClick={() => setDiscardPrompt(false)}>Keep editing</button><button type="button" onClick={onCancel}>Discard changes</button></div></section></div>}
    {mode === "edit" && preview && <section aria-label="Edit summary" className="rounded-8 border border-warning/40 bg-warning/10 p-12"><h2 className="m-0 text-14 font-medium">Review edit summary</h2><ul className="mb-0 mt-8 list-disc pl-20 text-12">{name !== initialState?.name && <li>Rename: native configuration files may lose the old server key.</li>}{transport !== initialState?.transport && <li>Transport change: transport-specific keys may be removed.</li>}<li>{preview.files.length} file(s) will change.</li></ul></section>}
    {pendingApproval !== null && <McpLaunchConfirmDialog
      approval={pendingApproval}
      onRun={() => void confirmLaunch()}
      onCancel={() => setPendingApproval(null)}
    />}
    {renameConfirm && <div role="dialog" aria-modal="true" aria-labelledby="mcp-rename-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="grid max-w-[520px] gap-12 rounded-12 border border-border bg-surface p-20"><h2 id="mcp-rename-title" className="m-0 text-18">Rename “{initialState?.name}” to “{name.trim()}”?</h2><p className="m-0 text-13 text-muted">Each file below loses the old server key and gains the new one.</p><ul className="mb-0 grid gap-4 pl-20 text-12 text-text-secondary">{renameConfirm.files.map((file) => <li key={`${file.agent}:${file.path}`}>{file.agent} · {file.path} loses “{initialState?.name}” and gains “{name.trim()}”.</li>)}</ul><div className="flex justify-end gap-8"><button type="button" onClick={() => setRenameConfirm(null)} className="rounded-8 border border-border px-12 py-8 text-13">Cancel</button><button type="button" disabled={saving} onClick={() => void applyReviewedPlan(renameConfirm)} className="rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent disabled:opacity-50">Apply</button></div></section></div>}
    {conflict && <div role="dialog" aria-modal="true" aria-labelledby="mcp-conflict-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40 p-16"><section className="grid max-w-[460px] gap-12 rounded-12 border border-border bg-surface p-20"><h2 id="mcp-conflict-title" className="m-0 text-18">This server changed outside AgenTuna</h2><p className="m-0 text-13 text-muted">A reviewed file changed while you were editing. Keep your edits or reload the server as it is now.</p><div className="flex justify-end gap-8"><button type="button" onClick={() => void replan(false)} className="rounded-8 border border-border px-12 py-8 text-13">Reload</button><button type="button" onClick={() => void replan(true)} className="rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent">Keep mine (re-apply)</button></div></section></div>}
  </main>;
}
