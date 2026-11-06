import { useEffect, useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AddSkillWizard as WizardState } from "../../generated/AddSkillWizard";
import type { SourceDetails } from "../../generated/SourceDetails";
import type { SourceKind } from "../../generated/SourceKind";
import type { WizardEffect } from "../../generated/WizardEffect";
import type { WizardEvent } from "../../generated/WizardEvent";
import type { WizardStep } from "../../generated/WizardStep";
import type { SkillAddApplyResultView } from "../../generated/SkillAddApplyResultView";
import type { SkillAddPreviewRequest } from "../../generated/SkillAddPreviewRequest";
import type { SkillAddPreviewView } from "../../generated/SkillAddPreviewView";

const VALIDATION_MESSAGE = {
  source_required: "Choose a skill source.",
  details_required: "Complete the required source details.",
  name_required: "Enter a skill name.",
  scope_required: "Choose a destination scope.",
  agent_required: "Select at least one agent.",
  imported_skill_required: "Select a valid imported skill.",
} as const;

const SOURCES: readonly { id: SourceKind; label: string; description: string; icon: string }[] = [
  { id: "new", label: "Write a new skill", description: "Start from a SKILL.md template and fill in name, trigger and steps.", icon: "＋" },
  { id: "git", label: "Import from git", description: "Paste a repo URL. AgenTuna lists every skill in it; pick the ones you want.", icon: "⑂" },
  { id: "library", label: "From your library", description: "Reuse a skill from another scope or another project on this Mac.", icon: "▤" },
];

const MORE_SOURCES: readonly { id: SourceKind; label: string }[] = [
  { id: "marketplace", label: "Marketplace" },
  { id: "folder", label: "Local folder" },
  { id: "zip", label: "Zip file" },
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

/** Add-skill flow driven by the Rust typed state machine and reviewed file plan. */
export function AddSkillWizard({ onClose, projectId = null }: { readonly onClose: () => void; readonly projectId?: string | null }): JSX.Element {
  const [wizard, setWizard] = useState<WizardState>(EMPTY);
  const wizardRef = useRef(EMPTY);
  const queue = useRef(Promise.resolve());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [discardConfirm, setDiscardConfirm] = useState(false);
  const [preview, setPreview] = useState<SkillAddPreviewView | null>(null);
  const [stageRequested, setStageRequested] = useState(false);
  const [confirmOverwrites, setConfirmOverwrites] = useState(false);
  const [outcome, setOutcome] = useState<SkillAddApplyResultView | null>(null);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        if (discardConfirm) setDiscardConfirm(false);
        else void send({ type: "cancel" });
      } else if (event.key === "Enter" && !event.isComposing && !busy && !discardConfirm) {
        const active = document.activeElement;
        if (active instanceof HTMLTextAreaElement || active instanceof HTMLButtonElement && !active.matches("[data-source-card]") || active instanceof HTMLSelectElement) return;
        const next = document.querySelector<HTMLButtonElement>('button[data-wizard-next="true"]');
        if (next && !next.disabled) {
          event.preventDefault();
          next.click();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [busy, discardConfirm]);

  const send = (event: WizardEvent) => {
    const operation = queue.current.then(async () => {
      setBusy(true);
      setError(null);
      let applying = false;
      try {
        const [next, effects] = await invoke<[WizardState, WizardEffect[]]>("skill_wizard_transition", { wizard: wizardRef.current, event });
        wizardRef.current = next;
        setWizard(next);
        for (const effect of effects) {
          if (effect.type === "confirm_discard") setDiscardConfirm(true);
          else if (effect.type === "discard_draft") onClose();
          else if (effect.type === "validation_failed") setError(VALIDATION_MESSAGE[effect.payload.issue]);
          else if (effect.type === "fetch_source") setError("Choose a source, then continue to review its available content.");
          else if (effect.type === "ready_to_import") {
            applying = true;
            if (!preview) {
              setError("The review preview is unavailable. Go back and reopen Review.");
              continue;
            }
            const result = await invoke<SkillAddApplyResultView>("skill_add_apply", {
              request: { preview_id: preview.preview_id, confirm_overwrites: confirmOverwrites },
            });
            setOutcome(result);
            const [done] = await invoke<[WizardState, WizardEffect[]]>("skill_wizard_transition", {
              wizard: next,
              event: { type: "import_completed" },
            });
            wizardRef.current = done;
            setWizard(done);
          }
        }
        if (event.type === "next" && next.step === "review" && !effects.some((effect) => effect.type === "ready_to_import")) {
          setPreview(null);
          setConfirmOverwrites(false);
          const request: SkillAddPreviewRequest = {
            wizard: next,
            project_id: projectId,
            stage_requested: stageRequested,
          };
          setPreview(await invoke<SkillAddPreviewView>("skill_add_preview", { request }));
        }
      } catch (reason) {
        const value = reason as { message?: unknown; refusal?: { remedies?: string[] } } | null;
        const message = typeof value?.message === "string" ? value.message : reason instanceof Error ? reason.message : "The reviewed skill plan could not be applied.";
        const remedies = value?.refusal?.remedies;
        if (applying) {
          await refreshPreview(stageRequested);
          setError(`${message}${remedies?.length ? ` Remedies: ${remedies.join(", ")}.` : ""} Review the refreshed plan before trying again.`);
        } else {
          setError(remedies?.length ? `${message} Remedies: ${remedies.join(", ")}.` : message);
        }
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
  const detailsReady = (() => {
    const details = wizard.details;
    switch (wizard.source) {
      case "new": return Boolean(details.template?.trim());
      case "git": return /^(https:\/\/|ssh:\/\/|git@)/.test(details.repo_url.trim()) && Boolean(details.fetched_repo_selection?.trim());
      case "library": return Boolean(details.library_selection?.trim());
      case "marketplace": return details.marketplace_id.includes("/") && details.marketplace_id.split("/").every(Boolean);
      case "folder": return Boolean(details.folder_path.trim());
      case "zip": return Boolean(details.zip_path.trim());
      default: return false;
    }
  })();
  const canAdvance = wizard.step === "source" ? wizard.source !== null
    : wizard.step === "details" ? detailsReady && Boolean(wizard.name.trim())
    : wizard.step === "scope_agents" ? Boolean(wizard.scope) && wizard.agents.length > 0
    : wizard.step === "review" ? Boolean(preview && (!preview.requires_overwrite_confirmation || confirmOverwrites))
    : false;

  const refreshPreview = async (stage: boolean) => {
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      const request: SkillAddPreviewRequest = { wizard: wizardRef.current, project_id: projectId, stage_requested: stage };
      setPreview(await invoke<SkillAddPreviewView>("skill_add_preview", { request }));
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setError(typeof value?.message === "string" ? value.message : "The skill review could not be refreshed.");
    } finally {
      setBusy(false);
    }
  };

  return <main className="mx-auto flex w-full max-w-[1240px] flex-col gap-20 p-24" aria-labelledby={wizard.step === "done" ? undefined : "add-skill-title"} aria-label={wizard.step === "done" ? "Add skill result" : undefined}>
    {wizard.step !== "done" && <header className="flex items-start justify-between gap-16">
      <div><p className="m-0 text-12 font-medium uppercase tracking-wide text-muted">Skills / Add</p><h1 id="add-skill-title" className="mb-0 mt-4 text-24 font-semibold text-text">Add a skill</h1><p className="mb-0 mt-6 text-14 text-muted">Choose a source, review its details, then select where to install it.</p></div>
      <button type="button" className="rounded-8 border border-border px-12 py-8 text-13 text-text" onClick={() => void send({ type: "cancel" })}>Cancel</button>
    </header>}
    {wizard.step !== "done" && <nav aria-label="Wizard progress" className="grid max-w-[760px] grid-cols-4 gap-8">
      {(["source", "details", "scope_agents", "review"] as WizardStep[]).map((step, index) => <div key={step} aria-current={wizard.step === step ? "step" : undefined} className={`border-b-2 pb-8 text-12 ${wizard.step === step ? "border-accent font-semibold text-text" : "border-border text-muted"}`}><span className="mr-6">{index + 1}.</span>{step === "scope_agents" ? "Scope & agents" : step[0]?.toUpperCase() + step.slice(1)}</div>)}
    </nav>}
    {error && <div role="alert" className="rounded-8 border border-danger/40 bg-danger/10 px-12 py-10 text-13 text-text">{error}</div>}
    {wizard.step === "source" && <section className="flex flex-col gap-16" aria-labelledby="source-heading"><h2 id="source-heading" className="m-0 text-18 font-semibold text-text">Where does the skill come from?</h2><div className="grid grid-cols-1 gap-14 md:grid-cols-3">{SOURCES.map((source) => <button type="button" data-source-card="true" key={source.id} aria-pressed={wizard.source === source.id} onClick={() => void send({ type: "select_source", payload: source.id })} className={`flex min-h-[330px] flex-col items-start rounded-12 border p-6 text-left ${wizard.source === source.id ? "border-accent bg-accent/10" : "border-border bg-surface"} focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent`}><span aria-hidden="true" className="grid h-12 w-12 place-items-center rounded-8 bg-accent/10 text-18 text-accent">{source.icon}</span><span className="mt-4 block text-17 font-semibold text-text">{source.label}</span><span className="mt-2 block text-13 text-muted">{source.description}</span><span className="mt-auto pt-6 text-13 font-semibold text-accent">Choose →</span></button>)}</div><details className="text-13 text-muted"><summary className="w-fit cursor-pointer">More import options</summary><div className="mt-8 flex flex-wrap gap-8">{MORE_SOURCES.map((source) => <button type="button" key={source.id} aria-pressed={wizard.source === source.id} onClick={() => void send({ type: "select_source", payload: source.id })} className={`rounded-8 border px-12 py-8 text-text ${wizard.source === source.id ? "border-accent bg-accent/10" : "border-border bg-surface"}`}>{source.label}</button>)}</div><p className="mb-0 mt-8">Marketplace availability follows your network policy.</p></details><p className="m-0 text-12 text-muted">Tip: skills are folders with a SKILL.md. AgenTuna copies them into each agent’s skills folder for the scope you pick.</p></section>}
    {wizard.step === "details" && <section className="flex flex-col gap-14" aria-labelledby="details-heading"><h2 id="details-heading" className="m-0 text-18 font-semibold text-text">{labelForSource} details</h2>
      {wizard.source === "new" && <label className="flex flex-col gap-5 text-13 text-text">Template<select aria-label="Template" value={wizard.details.template ?? ""} onChange={(event) => setDetails({ template: event.target.value || null })} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="">Choose a template</option><option value="blank">Basic skill</option><option value="checklist">Checklist</option><option value="workflow">Workflow guide</option></select></label>}
      {wizard.source === "git" && <><label className="flex flex-col gap-5 text-13 text-text">Repository URL<input aria-label="Repository URL" value={wizard.details.repo_url} onChange={(event) => setDetails({ repo_url: event.target.value, fetched_repo_selection: null })} placeholder="https://host/owner/repository.git" className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Branch, tag or commit (optional)<input value={wizard.details.repo_reference} onChange={(event) => setDetails({ repo_reference: event.target.value })} className="rounded-8 border border-border bg-surface px-10 py-8" /></label><label className="flex flex-col gap-5 text-13 text-text">Selected skill in repository<input aria-label="Repository skill selection" value={wizard.details.fetched_repo_selection ?? ""} onChange={(event) => setDetails({ fetched_repo_selection: event.target.value || null })} placeholder="Repository subpath" className="rounded-8 border border-border bg-surface px-10 py-8" /></label></>}
      {wizard.source === "library" && <label className="flex flex-col gap-5 text-13 text-text">Library skill<input aria-label="Library skill" value={wizard.details.library_selection ?? ""} onChange={(event) => setDetails({ library_selection: event.target.value || null })} placeholder="Choose a skill" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "marketplace" && <label className="flex flex-col gap-5 text-13 text-text">Marketplace ID<input aria-label="Marketplace ID" value={wizard.details.marketplace_id} onChange={(event) => setDetails({ marketplace_id: event.target.value })} placeholder="owner/skill-id" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "folder" && <label className="flex flex-col gap-5 text-13 text-text">Folder path<input aria-label="Folder path" value={wizard.details.folder_path} onChange={(event) => setDetails({ folder_path: event.target.value })} placeholder="Choose a folder" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      {wizard.source === "zip" && <label className="flex flex-col gap-5 text-13 text-text">ZIP archive path<input aria-label="ZIP path" value={wizard.details.zip_path} onChange={(event) => setDetails({ zip_path: event.target.value })} placeholder="Choose a .zip file" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>}
      <label className="flex flex-col gap-5 text-13 text-text">Skill name<input aria-label="Skill name" value={wizard.name} onChange={(event) => void send({ type: "set_name", payload: event.target.value })} placeholder="Name your skill" className="rounded-8 border border-border bg-surface px-10 py-8" /></label>
    </section>}
    {wizard.step === "scope_agents" && <section className="flex flex-col gap-12"><h2 className="m-0 text-18 font-semibold text-text">Choose destination</h2><label className="flex flex-col gap-5 text-13 text-text">Scope<select aria-label="Scope" value={wizard.scope ?? ""} onChange={(event) => void send({ type: "set_scope", payload: event.target.value })} className="rounded-8 border border-border bg-surface px-10 py-8"><option value="">Choose scope</option><option value="global">Global</option><option value="user">User</option><option value="project">Project</option><option value="local">Local (only you, this repo)</option></select></label><fieldset className="flex flex-col gap-8 rounded-8 border border-border p-12"><legend className="px-4 text-13 font-medium text-text">Agents</legend>{["Claude Code", "Codex", "omp"].map((agent) => <label key={agent} className="flex items-center gap-8 text-13 text-text"><input type="checkbox" checked={wizard.agents.includes(agent.toLowerCase().replace(" ", "_"))} onChange={(event) => { const id = agent.toLowerCase().replace(" ", "_"); const agents = event.target.checked ? [...wizard.agents, id] : wizard.agents.filter((entry) => entry !== id); void send({ type: "set_agents", payload: agents }); }} />{agent}</label>)}</fieldset></section>}
    {wizard.step === "review" && <section className="flex flex-col gap-12"><h2 className="m-0 text-18 font-semibold text-text">Review skill</h2><dl className="grid grid-cols-[140px_1fr] gap-10 rounded-10 border border-border p-16 text-13"><dt className="text-muted">Name</dt><dd className="m-0 text-text">{wizard.name}</dd><dt className="text-muted">Source</dt><dd className="m-0 text-text">{labelForSource}</dd><dt className="text-muted">Source selection</dt><dd className="m-0 text-text">{sourceSelection}</dd><dt className="text-muted">Scope</dt><dd className="m-0 text-text">{wizard.scope}</dd><dt className="text-muted">Agents</dt><dd className="m-0 text-text">{wizard.agents.join(", ")}</dd></dl>
      {preview ? <><dl className="grid grid-cols-[140px_1fr] gap-8 rounded-10 border border-border p-16 text-12"><dt className="text-muted">Canonical package</dt><dd className="m-0 break-all text-text">{preview.canonical_dir}</dd></dl><h3 className="m-0 text-14 font-semibold text-text">Files and deployments</h3><ul aria-label="Planned file paths" className="m-0 grid gap-6 p-0 text-12 text-text">{preview.files.map((file) => <li key={`${file.path}:${file.package_path}`} className="list-inside list-disc break-all">{file.path}{file.diff && <details className="ml-16"><summary className="cursor-pointer">Review existing-file diff</summary><pre className="overflow-auto whitespace-pre-wrap">{file.diff}</pre></details>}</li>)}</ul><ul aria-label="Planned deployments" className="m-0 grid gap-6 p-0 text-12 text-text">{preview.deployments.map((deployment) => <li key={deployment.agent} className="list-inside list-disc break-all">{deployment.agent}: {deployment.mode} → {deployment.path}</li>)}</ul>
        {preview.stage_available && <label className="flex items-center gap-8 text-13 text-text"><input type="checkbox" checked={stageRequested} onChange={(event) => { const stage = event.target.checked; setStageRequested(stage); void refreshPreview(stage); }} />Stage these project-scope paths with Git</label>}
        {preview.requires_overwrite_confirmation && <label className="flex items-start gap-8 text-13 text-text"><input type="checkbox" checked={confirmOverwrites} onChange={(event) => setConfirmOverwrites(event.target.checked)} />I reviewed the diffs and confirm replacing these existing files.</label>}
      </> : <p role="status" className="m-0 text-12 text-muted">Preparing the exact file plan…</p>}
    </section>}
    {wizard.step === "done" && outcome && <section role="status" className="mx-auto flex w-full max-w-[650px] flex-col items-center gap-14 py-24 text-center"><span aria-hidden="true" className="grid h-20 w-20 place-items-center rounded-full bg-success/15 text-success"><svg viewBox="0 0 24 24" className="h-10 w-10" fill="none" stroke="currentColor" strokeWidth="2.5"><path d="m5 12 4 4L19 6" /></svg></span><h2 className="m-0 text-24 font-semibold text-text">{outcome.skill_name} is ready</h2><div className="grid w-full gap-8 rounded-12 border border-border bg-surface p-16 text-left text-12 text-text"><p className="m-0 break-all">Operation: {outcome.operation_id}</p><ul aria-label="Written file paths" className="m-0 grid gap-6 p-0">{outcome.paths_written.map((path) => <li key={path} className="list-inside list-disc break-all">Wrote {path}</li>)}</ul><ul aria-label="Actual deployments" className="m-0 grid gap-6 p-0">{outcome.deployments.map((deployment) => <li key={deployment.agent} className="list-inside list-disc break-all">{deployment.agent} ({deployment.mode}): {deployment.path}</li>)}</ul><p className="m-0">{outcome.staged ? `Git staging: staged ${outcome.staged_paths.join(", ")}` : "Git staging: not staged"}</p>{outcome.stage_error && <p role="alert" className="m-0 text-danger">{outcome.stage_error}</p>}<p className="m-0 break-all text-muted">{outcome.backup_hint}</p></div></section>}
    {wizard.step === "done" ? <footer className="flex justify-center gap-8"><button type="button" onClick={() => { wizardRef.current = EMPTY; setWizard(EMPTY); setError(null); setPreview(null); setOutcome(null); setStageRequested(false); setConfirmOverwrites(false); }} className="rounded-8 border border-border px-14 py-8 text-13 text-text">Add another</button><button type="button" onClick={onClose} className="rounded-8 bg-accent px-14 py-8 text-13 font-medium text-on-accent">Back to Skills</button></footer> : <footer className="flex justify-between border-t border-border pt-14"><button type="button" disabled={wizard.step === "source" || busy} onClick={() => void send({ type: "back" })} className="rounded-8 border border-border px-14 py-8 text-13 text-text">Back</button><button type="button" data-wizard-next="true" disabled={busy || !canAdvance} onClick={() => void send({ type: "next" })} className="rounded-8 bg-accent px-14 py-8 text-13 font-medium text-on-accent disabled:opacity-50">{wizard.step === "review" ? "Import skill" : "Next"}</button></footer>}
    {discardConfirm && <div role="alertdialog" aria-modal="true" aria-labelledby="discard-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40"><div className="w-full max-w-[380px] rounded-12 border border-border bg-surface p-20"><h2 id="discard-title" className="m-0 text-18 font-semibold text-text">Discard this skill draft?</h2><p className="text-13 text-muted">Your entered details will be lost. No files have been written.</p><div className="flex justify-end gap-8"><button type="button" onClick={() => setDiscardConfirm(false)} className="rounded-8 border border-border px-12 py-8 text-13">Keep editing</button><button type="button" onClick={() => { setDiscardConfirm(false); void send({ type: "confirm_discard" }); }} className="rounded-8 bg-danger px-12 py-8 text-13 text-white">Discard</button></div></div></div>}
  </main>;
}
