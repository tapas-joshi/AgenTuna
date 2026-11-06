import { useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AddSkillWizard as WizardState } from "../../generated/AddSkillWizard";
import type { SourceDetails } from "../../generated/SourceDetails";
import type { SourceKind } from "../../generated/SourceKind";
import type { WizardEffect } from "../../generated/WizardEffect";
import type { WizardEvent } from "../../generated/WizardEvent";
import type { WizardStep } from "../../generated/WizardStep";

const VALIDATION_MESSAGE = {
  source_required: "Choose a skill source.",
  details_required: "Complete the required source details.",
  name_required: "Enter a skill name.",
  scope_required: "Choose a destination scope.",
  agent_required: "Select at least one agent.",
  imported_skill_required: "Select a valid imported skill.",
} as const;

const SOURCES: readonly { id: SourceKind; label: string; description: string }[] = [
  { id: "new", label: "Create new", description: "Start from a template" },
  { id: "git", label: "Git repository", description: "Clone a repository" },
  { id: "library", label: "Library", description: "Copy an existing skill" },
  { id: "marketplace", label: "Marketplace", description: "Install a listed skill" },
  { id: "folder", label: "Folder", description: "Import a local folder" },
  { id: "zip", label: "ZIP archive", description: "Import a ZIP file" },
];

const EMPTY: WizardState = {
  step: "source",
  source: null,
  details: { repo_url: "", repo_reference: "", library_selection: null, marketplace_id: "", folder_path: "", zip_path: "", template: null, fetched_repo_selection: null },
  name: "",
  scope: null,
  agents: [],
  completed: false,
};

/** Add-skill flow driven by the Rust typed state machine. */
export function AddSkillWizard({ onClose }: { readonly onClose: () => void }): JSX.Element {
  const [wizard, setWizard] = useState<WizardState>(EMPTY);
  const wizardRef = useRef(EMPTY);
  const queue = useRef(Promise.resolve());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [discardConfirm, setDiscardConfirm] = useState(false);

  const send = (event: WizardEvent) => {
    const operation = queue.current.then(async () => {
      setBusy(true);
      setError(null);
      try {
        const [next, effects] = await invoke<[WizardState, WizardEffect[]]>("skill_wizard_transition", { wizard: wizardRef.current, event });
        wizardRef.current = next;
        setWizard(next);
        for (const effect of effects) {
          if (effect.type === "confirm_discard") setDiscardConfirm(true);
          else if (effect.type === "discard_draft") onClose();
          else if (effect.type === "validation_failed") setError(VALIDATION_MESSAGE[effect.payload.issue]);
          else if (effect.type === "fetch_source") setError("This source needs a preview provider before it can continue.");
          else if (effect.type === "ready_to_import") setError("The review is valid. The import provider must succeed before the wizard can finish.");
        }
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : "The wizard could not update.");
      } finally {
        setBusy(false);
      }
    });
    queue.current = operation.catch(() => undefined);
    return operation;
  };

  const setDetails = (patch: Partial<SourceDetails>) => void send({ type: "set_details", payload: { ...wizardRef.current.details, ...patch } });
  const labelForSource = SOURCES.find(({ id }) => id === wizard.source)?.label ?? "Choose a source";
  const sourceSelection = wizard.details.fetched_repo_selection || wizard.details.library_selection || wizard.details.marketplace_id || wizard.details.folder_path || wizard.details.zip_path || wizard.details.template || wizard.details.repo_url || "Not selected";

  return <main className="mx-auto flex w-full max-w-[920px] flex-col gap-20 p-24" aria-labelledby="add-skill-title">
    <header className="flex items-start justify-between gap-16">
      <div><p className="m-0 text-12 font-medium uppercase tracking-wide text-muted">Skills / Add</p><h1 id="add-skill-title" className="mb-0 mt-4 text-24 font-semibold text-text">Add a skill</h1><p className="mb-0 mt-6 text-14 text-muted">Choose a source, review its details, then select where to install it.</p></div>
      <button type="button" className="rounded-8 border border-border px-12 py-8 text-13 text-text" onClick={() => void send({ type: "cancel" })}>Cancel</button>
    </header>
    <nav aria-label="Wizard progress" className="grid grid-cols-5 gap-8">
      {(["source", "details", "scope_agents", "review", "done"] as WizardStep[]).map((step, index) => <div key={step} className={`border-b-2 pb-8 text-12 ${wizard.step === step ? "border-accent font-semibold text-text" : "border-border text-muted"}`}><span className="mr-6">{index + 1}.</span>{step === "scope_agents" ? "Scope & agents" : step[0]?.toUpperCase() + step.slice(1)}</div>)}
    </nav>
    {error && <div role="alert" className="rounded-8 border border-danger/40 bg-danger/10 px-12 py-10 text-13 text-text">{error}</div>}
    {wizard.step === "source" && <section className="flex flex-col gap-12" aria-labelledby="source-heading"><h2 id="source-heading" className="m-0 text-18 font-semibold text-text">Where is the skill coming from?</h2><div className="grid grid-cols-2 gap-10 md:grid-cols-3">{SOURCES.map((source) => <button type="button" key={source.id} aria-pressed={wizard.source === source.id} onClick={() => void send({ type: "select_source", payload: source.id })} className={`min-h-[86px] rounded-10 border p-12 text-left ${wizard.source === source.id ? "border-accent bg-accent/10" : "border-border bg-surface"}`}><span className="block text-14 font-semibold text-text">{source.label}</span><span className="mt-5 block text-12 text-muted">{source.description}</span></button>)}</div></section>}
    {wizard.step === "details" && <section className="flex flex-col gap-14" aria-labelledby="details-heading"><h2 id="details-heading" className="m-0 text-18 font-semibold text-text">{labelForSource} details</h2>
      {wizard.source === "new" && <label className="flex flex-col gap-5 text-13 text-text">Template<select aria-label="Template" value={wizard.details.template ?? ""} onChange={(event) => setDetails({ template: event.target.value || null })} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="">Choose a template</option><option value="basic">Basic skill</option><option value="workflow">Workflow guide</option></select></label>}
      {wizard.source === "git" && <><label className="flex flex-col gap-5 text-13 text-text">Repository URL<input aria-label="Repository URL" value={wizard.details.repo_url} onChange={(event) => setDetails({ repo_url: event.target.value, fetched_repo_selection: null })} placeholder="https://host/owner/repository.git" className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Branch, tag or commit (optional)<input value={wizard.details.repo_reference} onChange={(event) => setDetails({ repo_reference: event.target.value })} className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Selected skill in repository<input aria-label="Repository skill selection" value={wizard.details.fetched_repo_selection ?? ""} onChange={(event) => setDetails({ fetched_repo_selection: event.target.value || null })} placeholder="Repository subpath" className="rounded-8 border border-border bg-surface px-10 py-8" /></label></>}
      {wizard.source === "library" && <label className="flex flex-col gap-5 text-13 text-text">Library skill<input aria-label="Library skill" value={wizard.details.library_selection ?? ""} onChange={(event) => setDetails({ library_selection: event.target.value || null })} placeholder="Choose a skill" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "marketplace" && <label className="flex flex-col gap-5 text-13 text-text">Marketplace ID<input aria-label="Marketplace ID" value={wizard.details.marketplace_id} onChange={(event) => setDetails({ marketplace_id: event.target.value })} placeholder="owner/skill-id" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "folder" && <label className="flex flex-col gap-5 text-13 text-text">Folder path<input aria-label="Folder path" value={wizard.details.folder_path} onChange={(event) => setDetails({ folder_path: event.target.value })} placeholder="Choose a folder" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "zip" && <label className="flex flex-col gap-5 text-13 text-text">ZIP archive path<input aria-label="ZIP path" value={wizard.details.zip_path} onChange={(event) => setDetails({ zip_path: event.target.value })} placeholder="Choose a .zip file" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      <label className="flex flex-col gap-5 text-13 text-text">Skill name<input aria-label="Skill name" value={wizard.name} onChange={(event) => void send({ type: "set_name", payload: event.target.value })} placeholder="Name your skill" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>
    </section>}
    {wizard.step === "scope_agents" && <section className="flex flex-col gap-12"><h2 className="m-0 text-18 font-semibold text-text">Choose destination</h2><label className="flex flex-col gap-5 text-13 text-text">Scope<select aria-label="Scope" value={wizard.scope ?? ""} onChange={(event) => void send({ type: "set_scope", payload: event.target.value })} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="">Choose scope</option><option value="global">Global</option><option value="user">User</option><option value="project">Project</option><option value="local">Local (only you, this repo)</option></select></label><fieldset className="flex flex-col gap-8 rounded-8 border border-border p-12"><legend className="px-4 text-13 font-medium text-text">Agents</legend>{["Claude Code", "Codex", "omp"].map((agent) => <label key={agent} className="flex items-center gap-8 text-13 text-text"><input type="checkbox" checked={wizard.agents.includes(agent.toLowerCase().replace(" ", "_"))} onChange={(event) => { const id = agent.toLowerCase().replace(" ", "_"); const agents = event.target.checked ? [...wizard.agents, id] : wizard.agents.filter((entry) => entry !== id); void send({ type: "set_agents", payload: agents }); }} />{agent}</label>)}</fieldset></section>}
    {wizard.step === "review" && <section className="flex flex-col gap-12"><h2 className="m-0 text-18 font-semibold text-text">Review skill</h2><dl className="grid grid-cols-[140px_1fr] gap-10 rounded-10 border border-border p-16 text-13"><dt className="text-muted">Name</dt><dd className="m-0 text-text">{wizard.name}</dd><dt className="text-muted">Source</dt><dd className="m-0 text-text">{labelForSource}</dd><dt className="text-muted">Source selection</dt><dd className="m-0 text-text">{sourceSelection}</dd><dt className="text-muted">Scope</dt><dd className="m-0 text-text">{wizard.scope}</dd><dt className="text-muted">Agents</dt><dd className="m-0 text-text">{wizard.agents.join(", ")}</dd></dl><p className="m-0 text-12 text-muted">Review the source, name and selected deployment targets before continuing.</p></section>}
    {wizard.step === "done" && <section role="status" className="rounded-10 border border-success/40 bg-success/10 p-20"><h2 className="m-0 text-18 font-semibold text-text">Skill added</h2><p className="mb-0 mt-8 text-13 text-muted">{wizard.name} is ready in {wizard.scope} scope.</p></section>}
    <footer className="flex justify-between border-t border-border pt-14"><button type="button" disabled={wizard.step === "source" || wizard.step === "done" || busy} onClick={() => void send({ type: "back" })} className="rounded-8 border border-border px-14 py-8 text-13 text-text">Back</button>{wizard.step === "done" ? <button type="button" onClick={onClose} className="rounded-8 bg-accent px-14 py-8 text-13 font-medium text-on-accent">Close</button> : <button type="button" disabled={busy || wizard.step === "source" && wizard.source === null} onClick={() => void send({ type: "next" })} className="rounded-8 bg-accent px-14 py-8 text-13 font-medium text-on-accent disabled:opacity-50">{wizard.step === "review" ? "Add skill" : "Next"}</button>}</footer>
    {discardConfirm && <div role="alertdialog" aria-modal="true" aria-labelledby="discard-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40"><div className="w-full max-w-[380px] rounded-12 border border-border bg-surface p-20"><h2 id="discard-title" className="m-0 text-18 font-semibold text-text">Discard this skill draft?</h2><p className="text-13 text-muted">Your entered details will be lost. No files have been written.</p><div className="flex justify-end gap-8"><button type="button" onClick={() => setDiscardConfirm(false)} className="rounded-8 border border-border px-12 py-8 text-13">Keep editing</button><button type="button" onClick={() => { setDiscardConfirm(false); void send({ type: "confirm_discard" }); }} className="rounded-8 bg-danger px-12 py-8 text-13 text-white">Discard</button></div></div></div>}
  </main>;
}
