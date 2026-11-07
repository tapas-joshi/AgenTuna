import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { JSX } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AddSkillWizard as WizardState } from "../../generated/AddSkillWizard";
import type { SourceDetails } from "../../generated/SourceDetails";
import type { SkillImportCandidateView } from "../../generated/SkillImportCandidateView";
import type { NetworkPolicyView } from "../../generated/NetworkPolicyView";
import type { MarketplaceSearchView } from "../../generated/MarketplaceSearchView";
import type { SkillImportCandidatesView } from "../../generated/SkillImportCandidatesView";
import type { SourceKind } from "../../generated/SourceKind";
import type { WizardEffect } from "../../generated/WizardEffect";
import type { WizardEvent } from "../../generated/WizardEvent";
import type { WizardStep } from "../../generated/WizardStep";
import type { SkillAddApplyResultView } from "../../generated/SkillAddApplyResultView";
import type { SkillAddApplyJobStartView } from "../../generated/SkillAddApplyJobStartView";
import type { SkillAddPreviewRequest } from "../../generated/SkillAddPreviewRequest";
import type { SkillAddPreviewView } from "../../generated/SkillAddPreviewView";
import type { SkillLibraryCatalogView } from "../../generated/SkillLibraryCatalogView";
import type { JobFinishedEvent } from "../../generated/JobFinishedEvent";
import type { JobId } from "../../generated/JobId";
import type { JobProgressEvent } from "../../generated/JobProgressEvent";
import type { SkillImportJobStartView } from "../../generated/SkillImportJobStartView";
import type { LibraryTransferMode } from "../../generated/LibraryTransferMode";
import type { AgentCardView } from "../../generated/AgentCardView";
import type { SkillScopeOptionsRequest } from "../../generated/SkillScopeOptionsRequest";
import type { SkillScopeOptionsView } from "../../generated/SkillScopeOptionsView";
import { ConnectAgentDialog } from "../../overlays/dialogs/ConnectAgentDialog";
import type { ScopeToken } from "../../theme";

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

/** Stepper labels and order, matching the four designed steps. */
const STEP_LABELS: readonly { readonly id: WizardStep; readonly label: string }[] = [
  { id: "source", label: "Source" },
  { id: "details", label: "Details" },
  { id: "scope_agents", label: "Scope & agents" },
  { id: "review", label: "Review" },
];

/** Display names for agent ids in done-step copy. */
const AGENT_LABELS: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  codex: "Codex",
  omp: "omp",
  cursor: "Cursor",
};

const EMPTY: WizardState = {
  step: "source",
  source: null,
  details: { repo_url: "", repo_reference: "", library_selections: [], library_mode: "copy", marketplace_id: "", folder_path: "", zip_path: "", description: "", template: null, fetched_repo_selections: [], folder_selections: [], zip_selections: [] },
  name: "",
  scope: null,
  agents: [],
  completed: false,
};

function emptyForScope(scope: ScopeToken): WizardState {
  return { ...structuredClone(EMPTY), scope };
}

/** Formats an install count the way the marketplace table promises, for example `1.2k`. */
function formatInstalls(value: number | bigint): string {
  const num = Number(value);
  if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1).replace(/\.0$/, "")}m`;
  if (num >= 1_000) return `${(num / 1_000).toFixed(1).replace(/\.0$/, "")}k`;
  return String(value);
}

type SkillImportSource = "git" | "folder" | "zip";
type ActiveSkillImport = SkillImportJobStartView & { readonly source: SkillImportSource };
type SkillImportSessionRef = { readonly id: string; readonly source: SkillImportSource };

/** Add-skill flow driven by the Rust typed state machine and reviewed file plan. */
export function AddSkillWizard({ onClose, projectId = null, initialScope = "user", initialSource = null }: { readonly onClose: () => void; readonly projectId?: string | null; readonly initialScope?: ScopeToken; /** Source chosen by the button that opened the wizard; skips the source picker. */ readonly initialSource?: SourceKind | null }): JSX.Element {
  // A named entry point ("Import from git") opens the form for that source;
  // the source step stays reachable through Back.
  const initialState: WizardState = initialSource === null
    ? emptyForScope(initialScope)
    : { ...emptyForScope(initialScope), step: "details", source: initialSource };
  const [wizard, setWizard] = useState<WizardState>(initialState);
  const wizardRef = useRef(initialState);
  const detailsDraftRef = useRef(initialState.details);
  const queue = useRef(Promise.resolve());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [discardConfirm, setDiscardConfirm] = useState(false);
  const [preview, setPreview] = useState<SkillAddPreviewView | null>(null);
  const [templatePreview, setTemplatePreview] = useState<{ slug: string | null; markdown: string; name_error: string | null; description_error: string | null } | null>(null);
  const [importCandidates, setImportCandidates] = useState<SkillImportCandidateView[]>([]);
  const [candidateError, setCandidateError] = useState<string | null>(null);
  const [activeImport, setActiveImport] = useState<ActiveSkillImport | null>(null);
  const activeImportRef = useRef<ActiveSkillImport | null>(null);
  const [importProgress, setImportProgress] = useState<JobProgressEvent | null>(null);
  const [importSessionId, setImportSessionId] = useState<string | null>(null);
  const importSessionRef = useRef<SkillImportSessionRef | null>(null);
  const queuedProgress = useRef(new Map<JobId, JobProgressEvent>());
  const queuedFinished = useRef(new Map<JobId, JobFinishedEvent>());
  const [candidateStatus, setCandidateStatus] = useState<string | null>(null);
  const [previewedCandidateId, setPreviewedCandidateId] = useState<string | null>(null);
  const candidateListRef = useRef<HTMLDivElement | null>(null);
  const [scopeOptions, setScopeOptions] = useState<SkillScopeOptionsView | null>(null);
  const [scopeOptionsBusy, setScopeOptionsBusy] = useState(false);
  const [scopeOptionsError, setScopeOptionsError] = useState<string | null>(null);
  const [scopeRefreshVersion, setScopeRefreshVersion] = useState(0);
  const [localScopeExpanded, setLocalScopeExpanded] = useState(false);
  const [pathAgent, setPathAgent] = useState<AgentCardView | null>(null);
  const [agentPathLoading, setAgentPathLoading] = useState(false);
  const candidateScrollTop = useRef(0);
  const [libraryCatalog, setLibraryCatalog] = useState<SkillLibraryCatalogView | null>(null);
  const [librarySourceId, setLibrarySourceId] = useState("");
  const [libraryCatalogError, setLibraryCatalogError] = useState<string | null>(null);
  useEffect(() => {
    if (wizard.step !== "details" || wizard.source !== "library") return;
    let current = true;
    setLibraryCatalog(null);
    setLibraryCatalogError(null);
    void invoke<SkillLibraryCatalogView>("skill_library_catalog", {
      scope: wizard.scope ?? "user",
      projectId,
    }).then((catalog) => {
      if (!current) return;
      setLibraryCatalog(catalog);
      setLibrarySourceId((previous) => catalog.sources.some((source) => source.id === previous) ? previous : catalog.sources[0]?.id ?? "");
    }).catch((reason: unknown) => {
      if (!current) return;
      const value = reason as { message?: unknown } | null;
      setLibraryCatalogError(typeof value?.message === "string" ? value.message : "The skill library could not be read.");
    });
    return () => { current = false; };
  }, [wizard.step, wizard.source, wizard.scope, projectId]);
  const [marketplacePolicy, setMarketplacePolicy] = useState<NetworkPolicyView | null>(null);
  const [marketplaceQuery, setMarketplaceQuery] = useState("");
  const [marketplaceResults, setMarketplaceResults] = useState<MarketplaceSearchView | null>(null);
  const [marketplaceError, setMarketplaceError] = useState<string | null>(null);
  useEffect(() => {
    if (wizard.step !== "details" || wizard.source !== "marketplace") return;
    let current = true;
    void invoke<NetworkPolicyView>("network_policy")
      .then((policy) => { if (current) setMarketplacePolicy(policy); })
      .catch(() => { if (current) setMarketplaceError("Marketplace policy state is unavailable."); });
    return () => { current = false; };
  }, [wizard.step, wizard.source]);
  useEffect(() => {
    if (wizard.step !== "details" || wizard.source !== "new" || !wizard.details.template) {
      setTemplatePreview(null);
      return;
    }
    let current = true;
    const timer = window.setTimeout(() => {
      void invoke<{ slug: string | null; markdown: string; name_error: string | null; description_error: string | null }>("skill_template_preview", {
        name: wizard.name,
        description: wizard.details.description,
        template: wizard.details.template,
      }).then((result) => { if (current) setTemplatePreview(result); })
        .catch(() => { if (current) setTemplatePreview(null); });
    }, 120);
    return () => { current = false; window.clearTimeout(timer); };
  }, [wizard.step, wizard.source, wizard.name, wizard.details.description, wizard.details.template]);
  const [stageRequested, setStageRequested] = useState(false);
  const [confirmOverwrites, setConfirmOverwrites] = useState(false);
  const [outcome, setOutcome] = useState<SkillAddApplyResultView | null>(null);
  const [activeApplyJob, setActiveApplyJob] = useState<SkillAddApplyJobStartView | null>(null);
  const activeApplyJobRef = useRef<SkillAddApplyJobStartView | null>(null);
  const [applyProgress, setApplyProgress] = useState<JobProgressEvent | null>(null);
  const refreshPreviewRef = useRef<((stage: boolean) => Promise<void>) | null>(null);
  const releaseImportSession = useCallback((session: SkillImportSessionRef | null = importSessionRef.current) => {
    if (!session) return;
    if (importSessionRef.current?.id === session.id) {
      importSessionRef.current = null;
      setImportSessionId(null);
    }
    void invoke<boolean>("skill_import_release", { importId: session.id }).catch(() => undefined);
  }, []);
  const finishImportJob = useCallback(async (event: JobFinishedEvent) => {
    const job = activeImportRef.current;
    if (!job || job.job_id !== event.id) {
      queuedFinished.current.set(event.id, event);
      return;
    }
    if (event.state !== "succeeded") {
      activeImportRef.current = null;
      setActiveImport(null);
      setImportProgress(null);
      setCandidateError(event.state === "cancelled"
        ? "Import cancelled. You can retry without losing your wizard selections."
        : event.error?.kind === "failed" || event.error?.kind === "panic"
          ? event.error.message
          : "The import job did not complete.");
      setCandidateStatus(null);
      return;
    }
    setImportProgress({ id: event.id, name: event.name, fraction: 1, label: "Preparing imported skills…" });
    try {
      const result = await invoke<SkillImportCandidatesView>("skill_import_job_result", { importId: job.import_id });
      if (activeImportRef.current?.job_id !== event.id) return;
      releaseImportSession(importSessionRef.current);
      const session = { id: job.import_id, source: job.source };
      importSessionRef.current = session;
      setImportSessionId(session.id);
      const details = { ...detailsDraftRef.current };
      if (job.source === "git") details.fetched_repo_selections = [];
      else if (job.source === "folder") details.folder_selections = [];
      else details.zip_selections = [];
      detailsDraftRef.current = details;
      const next = { ...wizardRef.current, details };
      wizardRef.current = next;
      setWizard(next);
      setImportCandidates(result.candidates);
      setCandidateError(result.error);
      setCandidateStatus(result.error ?? (result.candidates.length === 0 ? job.source === "git" ? "No skills found. Check that this public repository contains a SKILL.md." : "No SKILL.md files were found in this source." : `${result.candidates.length} skill${result.candidates.length === 1 ? "" : "s"} ready to select.`));
      setPreviewedCandidateId(result.candidates.find((candidate) => candidate.name && !candidate.error)?.id ?? null);
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setCandidateError(typeof value?.message === "string" ? value.message : "Imported candidates could not be loaded.");
      setCandidateStatus(null);
    } finally {
      if (activeImportRef.current?.job_id === event.id) {
        activeImportRef.current = null;
        setActiveImport(null);
        setImportProgress(null);
      }
    }
  }, [releaseImportSession]);
  const finishApplyJob = useCallback(async (event: JobFinishedEvent) => {
    const job = activeApplyJobRef.current;
    if (!job || job.job_id !== event.id) {
      queuedFinished.current.set(event.id, event);
      return;
    }
    if (event.state !== "succeeded") {
      activeApplyJobRef.current = null;
      setActiveApplyJob(null);
      setApplyProgress(null);
      setConfirmOverwrites(false);
      const refresh = refreshPreviewRef.current;
      if (refresh) await refresh(stageRequested);
      else setPreview(null);
      const failure = event.error?.kind === "failed" || event.error?.kind === "panic"
        ? event.error.message
        : "The reviewed skill apply did not complete.";
      setError(`${failure} Review the refreshed plan before trying again.`);
      return;
    }
    setApplyProgress({ id: event.id, name: event.name, fraction: 1, label: "Loading the written skill result…" });
    try {
      const result = await invoke<SkillAddApplyResultView>("skill_add_apply_result", { resultId: job.result_id });
      if (activeApplyJobRef.current?.job_id !== event.id) return;
      releaseImportSession();
      setOutcome(result);
      const [done] = await invoke<[WizardState, WizardEffect[]]>("skill_wizard_transition", {
        wizard: wizardRef.current,
        event: { type: "import_completed" },
      });
      wizardRef.current = done;
      setWizard(done);
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setError(typeof value?.message === "string" ? value.message : "The skill was written, but its result could not be loaded.");
    } finally {
      if (activeApplyJobRef.current?.job_id === event.id) {
        activeApplyJobRef.current = null;
        setActiveApplyJob(null);
        setApplyProgress(null);
        setBusy(false);
      }
    }
  }, [releaseImportSession, stageRequested]);
  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    void listen<JobProgressEvent>("agentuna:job-progress", ({ payload }) => {
      if (activeImportRef.current?.job_id === payload.id) setImportProgress(payload);
      else if (activeApplyJobRef.current?.job_id === payload.id) setApplyProgress(payload);
      else queuedProgress.current.set(payload.id, payload);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    }).catch(() => { if (!disposed) setCandidateError("Import progress events are unavailable."); });
    void listen<JobFinishedEvent>("agentuna:job-finished", ({ payload }) => {
      if (activeImportRef.current?.job_id === payload.id) void finishImportJob(payload);
      else if (activeApplyJobRef.current?.job_id === payload.id) void finishApplyJob(payload);
      else queuedFinished.current.set(payload.id, payload);
    }).then((unlisten) => {
      if (disposed) unlisten();
      else unlisteners.push(unlisten);
    }).catch(() => { if (!disposed) setCandidateError("Import completion events are unavailable."); });
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
      const job = activeImportRef.current;
      if (job) void invoke<boolean>("jobs_cancel", { jobId: job.job_id }).catch(() => undefined);
      activeImportRef.current = null;
      const session = importSessionRef.current;
      importSessionRef.current = null;
      if (session) void invoke<boolean>("skill_import_release", { importId: session.id }).catch(() => undefined);
    };
  }, [finishImportJob, finishApplyJob]);
  useEffect(() => {
    if (wizard.step === "details" && wizard.source !== "new" && wizard.source !== "library" && wizard.source !== "marketplace") {
      const candidateList = candidateListRef.current;
      if (candidateList && typeof candidateList.scrollTo === "function") candidateList.scrollTo({ top: candidateScrollTop.current });
    }
  }, [wizard.step, wizard.source, importCandidates.length]);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        if (discardConfirm) setDiscardConfirm(false);
        else void send({ type: "cancel" });
      } else if (event.key === "Enter" && !event.isComposing && !busy && !activeImport && !discardConfirm) {
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
  }, [busy, activeImport, discardConfirm]);

  const send = (event: WizardEvent) => {
    const operation = queue.current.then(async () => {
      setBusy(true);
      setError(null);
      let applying = false;
      try {
        const [next, effects] = await invoke<[WizardState, WizardEffect[]]>("skill_wizard_transition", { wizard: wizardRef.current, event });
        wizardRef.current = next;
        if (event.type !== "set_details") detailsDraftRef.current = next.details;
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
            const job = await invoke<SkillAddApplyJobStartView>("skill_add_apply_start", {
              request: { preview_id: preview.preview_id, confirm_overwrites: confirmOverwrites },
            });
            activeApplyJobRef.current = job;
            setActiveApplyJob(job);
            setApplyProgress(queuedProgress.current.get(job.job_id) ?? null);
            queuedProgress.current.delete(job.job_id);
            const finished = queuedFinished.current.get(job.job_id);
            if (finished) {
              queuedFinished.current.delete(job.job_id);
              void finishApplyJob(finished);
            }
          }
        }
        if (event.type === "next" && next.step === "review" && !effects.some((effect) => effect.type === "ready_to_import")) {
          setPreview(null);
          setConfirmOverwrites(false);
          const request: SkillAddPreviewRequest = {
            wizard: next,
            project_id: projectId,
            import_id: importSessionId,
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
        if (!activeApplyJobRef.current) setBusy(false);
      }
    });
    queue.current = operation.catch(() => undefined);
    return operation;
  };

  const releaseActiveSkillImport = useCallback(() => {
    const job = activeImportRef.current;
    if (!job) return;
    setImportProgress({ id: job.job_id, name: "Skill import", fraction: null, label: "Cancelling import…" });
    void invoke<boolean>("jobs_cancel", { jobId: job.job_id }).catch((reason: unknown) => {
      const value = reason as { message?: unknown } | null;
      setCandidateError(typeof value?.message === "string" ? value.message : "The import could not be cancelled.");
    });
  }, []);
  const startSkillImport = async (source: SkillImportSource) => {
    if (activeImportRef.current) return;
    const details = wizardRef.current.details;
    const path = source === "folder" ? details.folder_path : details.zip_path;
    if (source !== "git" && !path.trim()) {
      setCandidateError(`Choose a ${source === "folder" ? "folder" : "ZIP archive"} before scanning.`);
      return;
    }
    if (source === "git" && !details.repo_url.trim()) {
      setCandidateError("Enter a repository URL before fetching.");
      return;
    }
    setBusy(true);
    setCandidateError(null);
    setCandidateStatus(null);
    try {
      const start = source === "git"
        ? await invoke<SkillImportJobStartView>("skill_import_git_start", {
          url: details.repo_url,
          reference: details.repo_reference || null,
        })
        : await invoke<SkillImportJobStartView>("skill_import_local_start", {
          path,
          kind: source,
          projectId,
        });
      const job = { ...start, source };
      activeImportRef.current = job;
      setActiveImport(job);
      setImportProgress(queuedProgress.current.get(job.job_id) ?? null);
      queuedProgress.current.delete(job.job_id);
      const finished = queuedFinished.current.get(job.job_id);
      if (finished) {
        queuedFinished.current.delete(job.job_id);
        void finishImportJob(finished);
      }
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setCandidateError(typeof value?.message === "string" ? value.message : "The import could not be started.");
    } finally {
      setBusy(false);
    }
  };
  const setDetails = (patch: Partial<SourceDetails>) => {
    const previous = detailsDraftRef.current;
    const invalidatedSource: SkillImportSource | null =
      patch.repo_url !== undefined && patch.repo_url !== previous.repo_url || patch.repo_reference !== undefined && patch.repo_reference !== previous.repo_reference ? "git"
      : patch.folder_path !== undefined && patch.folder_path !== previous.folder_path ? "folder"
      : patch.zip_path !== undefined && patch.zip_path !== previous.zip_path ? "zip"
      : null;
    if (invalidatedSource) {
      if (activeImportRef.current?.source === invalidatedSource) releaseActiveSkillImport();
      if (importSessionRef.current?.source === invalidatedSource) releaseImportSession();
      setImportCandidates([]);
      setCandidateError(null);
      setCandidateStatus(null);
      setPreviewedCandidateId(null);
    }
    const details = { ...previous, ...patch };
    detailsDraftRef.current = details;
    void send({ type: "set_details", payload: details });
  };
  const chooseImportPath = async (kind: "folder" | "zip") => {
    setBusy(true);
    setError(null);
    try {
      const path = await invoke<string | null>(kind === "folder" ? "skill_import_choose_folder" : "skill_import_choose_zip");
      if (path) setDetails(kind === "folder" ? { folder_path: path, folder_selections: [] } : { zip_path: path, zip_selections: [] });
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setError(typeof value?.message === "string" ? value.message : "The native file picker could not be opened.");
    } finally {
      setBusy(false);
    }
  };
  const chooseSource = (source: SourceKind) => {
    releaseActiveSkillImport();
    releaseImportSession();
    setImportCandidates([]);
    setCandidateError(null);
    setCandidateStatus(null);
    void send({ type: "select_source", payload: source });
  };
  const previousSourceRef = useRef(wizard.source);
  useEffect(() => {
    if (previousSourceRef.current !== wizard.source) {
      releaseActiveSkillImport();
      releaseImportSession();
      setImportCandidates([]);
      setCandidateError(null);
      setCandidateStatus(null);
    }
    previousSourceRef.current = wizard.source;
  }, [wizard.source, releaseImportSession, releaseActiveSkillImport]);
  const selectedImportIds = (source: SkillImportSource): string[] => {
    const details = detailsDraftRef.current;
    return source === "git" ? details.fetched_repo_selections : source === "folder" ? details.folder_selections : details.zip_selections;
  };
  const toggleImportCandidate = (source: SkillImportSource, id: string, checked: boolean) => {
    const selected = new Set(selectedImportIds(source));
    if (checked) {
      selected.add(id);
      setPreviewedCandidateId(id);
    } else selected.delete(id);
    const values = [...selected];
    if (source === "git") setDetails({ fetched_repo_selections: values });
    else if (source === "folder") setDetails({ folder_selections: values });
    else setDetails({ zip_selections: values });
  };
  const selectAllValidImportCandidates = (source: SkillImportSource) => {
    const selected = new Set(selectedImportIds(source));
    for (const candidate of importCandidates) {
      if (candidate.name && !candidate.error) selected.add(candidate.id);
    }
    const values = [...selected];
    if (source === "git") setDetails({ fetched_repo_selections: values });
    else if (source === "folder") setDetails({ folder_selections: values });
    else setDetails({ zip_selections: values });
    setPreviewedCandidateId(importCandidates.find((candidate) => candidate.name && !candidate.error)?.id ?? null);
  };
  const clearImportSelection = (source: SkillImportSource) => {
    if (source === "git") setDetails({ fetched_repo_selections: [] });
    else if (source === "folder") setDetails({ folder_selections: [] });
    else setDetails({ zip_selections: [] });
  };
  const previewedCandidate = importCandidates.find((candidate) => candidate.id === previewedCandidateId) ?? null;
  const previewCandidate = (candidate: SkillImportCandidateView | null) => {
    if (!candidate) return null;
    return <section aria-label="Selected skill preview" className="grid gap-4 rounded-8 border border-border p-12"><h3 className="m-0 text-13 font-semibold text-text">{candidate.name ?? "Skill"} · SKILL.md preview</h3>{candidate.markdown ? <pre className="m-0 max-h-[320px] overflow-auto whitespace-pre-wrap rounded-6 bg-surface p-8 text-11 text-text">{candidate.markdown}</pre> : <p className="m-0 text-12 text-muted">No SKILL.md preview is available.</p>}</section>;
  };
  const toggleLibrarySkill = (id: string, checked: boolean) => {
    const selected = new Set(detailsDraftRef.current.library_selections);
    if (checked) selected.add(id);
    else selected.delete(id);
    setDetails({ library_selections: [...selected] });
  };
  const selectAllAvailableLibrarySkills = () => {
    const selected = new Set(detailsDraftRef.current.library_selections);
    for (const skill of libraryCatalog?.skills ?? []) {
      if (skill.source_id === librarySourceId && !skill.error && !skill.clash_reason) selected.add(skill.id);
    }
    setDetails({ library_selections: [...selected] });
  };
  const searchMarketplace = async (refresh: boolean) => {
    setBusy(true);
    setMarketplaceError(null);
    try {
      const page = await invoke<MarketplaceSearchView>("skill_marketplace_search", {
        query: marketplaceQuery,
        refresh,
      });
      setMarketplaceResults(page);
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setMarketplaceError(typeof value?.message === "string" ? value.message : "Marketplace search failed.");
    } finally {
      setBusy(false);
    }
  };
  const labelForSource = SOURCES.find(({ id }) => id === wizard.source)?.label ?? "Choose a source";
  const libraryTransferMode: LibraryTransferMode | null = wizard.source === "library" ? wizard.details.library_mode : null;
  const selectedLibrarySkills = libraryCatalog?.skills.filter((skill) => wizard.details.library_selections.includes(skill.id)) ?? [];
  const selectedSkillNames = useMemo(() => {
    if (wizard.source === "new" || wizard.source === "marketplace") {
      return wizard.name.trim() ? [wizard.name.trim()] : [];
    }
    if (wizard.source === "library") {
      return (libraryCatalog?.skills ?? [])
        .filter((skill) => wizard.details.library_selections.includes(skill.id))
        .map((skill) => skill.name)
        .filter((name): name is string => Boolean(name));
    }
    const selectedIds = wizard.source === "git" ? wizard.details.fetched_repo_selections
      : wizard.source === "folder" ? wizard.details.folder_selections
      : wizard.source === "zip" ? wizard.details.zip_selections
      : [];
    const selected = new Set(selectedIds);
    return importCandidates
      .filter((candidate) => selected.has(candidate.id) && candidate.name !== null)
      .map((candidate) => candidate.name as string);
  }, [wizard.source, wizard.name, wizard.details, libraryCatalog, importCandidates]);
  useEffect(() => {
    if (wizard.step !== "scope_agents") return;
    let active = true;
    setScopeOptionsBusy(true);
    setScopeOptionsError(null);
    const request: SkillScopeOptionsRequest = {
      scope: wizard.scope,
      agents: [...wizard.agents],
      names: [...selectedSkillNames],
      project_id: projectId,
    };
    void invoke<SkillScopeOptionsView>("skill_scope_options", { request })
      .then((result) => { if (active) setScopeOptions(result); })
      .catch((cause: unknown) => {
        if (active) {
          const value = cause as { message?: unknown } | null;
          setScopeOptionsError(typeof value?.message === "string" ? value.message : "Scope and agent paths could not be loaded.");
          setScopeOptions(null);
        }
      })
      .finally(() => { if (active) setScopeOptionsBusy(false); });
    return () => { active = false; };
  }, [wizard.step, wizard.scope, wizard.agents, selectedSkillNames, projectId, scopeRefreshVersion]);
  const sourceSelection = (() => {
    if (wizard.source === "git" || wizard.source === "folder" || wizard.source === "zip") {
      const selected = importCandidates.filter((candidate) => selectedImportIds(wizard.source as SkillImportSource).includes(candidate.id));
      return selected.length ? selected.map((candidate) => candidate.name ?? candidate.id).join(", ") : wizard.source === "git" ? wizard.details.repo_url || "Not selected" : wizard.source === "folder" ? wizard.details.folder_path || "Not selected" : wizard.details.zip_path || "Not selected";
    }
    if (wizard.source === "library") return selectedLibrarySkills.length ? selectedLibrarySkills.map((skill) => skill.name ?? skill.id).join(", ") : "Not selected";
    return wizard.details.marketplace_id || wizard.details.template || "Not selected";
  })();
  const requiresName = wizard.source === "new" || wizard.source === "marketplace";
  const detailsReady = (() => {
    const details = wizard.details;
    switch (wizard.source) {
      case "new": return Boolean(details.template?.trim()) && details.description.length < 1024 && Boolean(templatePreview && !templatePreview.name_error && !templatePreview.description_error);
      case "git": return /^(https:\/\/|ssh:\/\/|git@)/.test(details.repo_url.trim()) && details.fetched_repo_selections.length > 0;
      case "library": return details.library_selections.length > 0;
      case "marketplace": return details.marketplace_id.includes("/") && details.marketplace_id.split("/").every(Boolean);
      case "folder": return Boolean(details.folder_path.trim()) && details.folder_selections.length > 0;
      case "zip": return Boolean(details.zip_path.trim()) && details.zip_selections.length > 0;
      default: return false;
    }
  })();
  const scopeHasBlockingClash = scopeOptions?.clashes.some((clash) => clash.blocking.length > 0) ?? false;
  const selectedAgentUnavailable = scopeOptions !== null && wizard.agents.some((id) => {
    const agent = scopeOptions.agents.find((entry) => entry.agent === id);
    return agent === undefined || agent.disabled_reason !== null;
  });
  const canAdvance = wizard.step === "source" ? wizard.source !== null
    : wizard.step === "details" ? detailsReady && (!requiresName || Boolean(wizard.name.trim())) && !busy && !activeImport
    : wizard.step === "scope_agents" ? Boolean(wizard.scope) && wizard.agents.length > 0 && scopeOptions !== null && !scopeOptionsBusy && !scopeHasBlockingClash && !selectedAgentUnavailable
    : wizard.step === "review" ? Boolean(preview && (!preview.requires_overwrite_confirmation || confirmOverwrites))
    : false;
  const setAgentPath = async (agentId: string) => {
    setAgentPathLoading(true);
    setScopeOptionsError(null);
    try {
      const agents = await invoke<AgentCardView[]>("agents_list");
      const agent = agents.find((candidate) => candidate.id === agentId);
      if (agent === undefined) {
        setScopeOptionsError("This agent is no longer available. Refresh the agent list and try again.");
      } else {
        setPathAgent(agent);
      }
    } catch (cause) {
      const value = cause as { message?: unknown } | null;
      setScopeOptionsError(typeof value?.message === "string" ? value.message : "The agent path settings could not be opened.");
    } finally {
      setAgentPathLoading(false);
    }
  };

  const refreshPreview = async (stage: boolean) => {
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      const request: SkillAddPreviewRequest = {
        wizard: wizardRef.current,
        project_id: projectId,
        import_id: importSessionId,
        stage_requested: stage,
      };
      setPreview(await invoke<SkillAddPreviewView>("skill_add_preview", { request }));
    } catch (reason) {
      const value = reason as { message?: unknown } | null;
      setError(typeof value?.message === "string" ? value.message : "The skill review could not be refreshed.");
    } finally {
      setBusy(false);
    }
  };
  refreshPreviewRef.current = refreshPreview;

  const currentStepIndex = STEP_LABELS.findIndex((step) => step.id === wizard.step);
  const stepTitle = wizard.step === "details"
    ? wizard.source === "library" ? "From your library"
      : wizard.source === "git" ? "Import from git"
      : wizard.source === "new" ? "Write a new skill"
      : "Source details"
    : wizard.step === "scope_agents" ? "Scope & agents"
    : wizard.step === "review" ? "Review"
    : "Add a skill";
  const stepSubtitle = wizard.step === "details"
    ? wizard.source === "library" ? "Pick skills that already exist in another scope or project on this Mac."
      : wizard.source === "git" ? "AgenTuna clones the repo into a temp folder and lists its skills."
      : wizard.source === "new" ? "Name it and say when an agent should use it. The description is what agents match on."
      : "Review the source details before choosing a scope."
    : wizard.step === "scope_agents" ? "Pick who gets the skill and which agents load it."
    : wizard.step === "review" ? "Nothing is written until you press Create skill."
    : "Choose where the skill comes from. Nothing is written until you review the plan.";
  const doneAgents = outcome === null ? [] : [...new Set(outcome.deployments.map((deployment) => AGENT_LABELS[deployment.agent] ?? deployment.agent))];
  return <main className="mx-auto flex w-full max-w-[1240px] flex-col gap-16 p-24" aria-labelledby={wizard.step === "done" ? undefined : "add-skill-title"} aria-label={wizard.step === "done" ? "Add skill result" : undefined}>
    {wizard.step !== "done" && <header className="flex items-start justify-between gap-16">
      <div><p className="m-0 text-12 font-medium text-muted">Skills / Add skill</p><h1 id="add-skill-title" className="mb-0 mt-4 text-26 font-semibold text-text">{stepTitle}</h1><p className="mb-0 mt-6 text-14 text-muted">{stepSubtitle}</p></div>
    </header>}
    {wizard.step !== "done" && <nav aria-label="Wizard progress" className="flex w-full max-w-[760px] items-center gap-8">
      {STEP_LABELS.map((step, index) => {
        const done = index < currentStepIndex;
        const current = index === currentStepIndex;
        return <div key={step.id} aria-current={current ? "step" : undefined} className="flex flex-1 items-center gap-8">
          <span aria-hidden="true" className={`grid size-24 shrink-0 place-items-center rounded-full border text-11 font-semibold ${done ? "border-success bg-success/15 text-success" : current ? "border-accent text-text" : "border-border text-text-muted"}`}>{done ? "✓" : index + 1}</span>
          <span className={`whitespace-nowrap text-12 ${current ? "font-semibold text-text" : done ? "text-text-secondary" : "text-text-muted"}`}>{step.label}</span>
          {index < STEP_LABELS.length - 1 && <span aria-hidden="true" className="h-px flex-1 bg-border" />}
        </div>;
      })}
    </nav>}
    {error && <div role="alert" className="rounded-8 border border-danger/40 bg-danger/10 px-12 py-8 text-13 text-text">{error}</div>}
    {wizard.step === "source" && <section className="flex flex-col gap-16" aria-label="Choose a skill source"><div className="grid grid-cols-1 gap-12 md:grid-cols-3">{SOURCES.map((source) => <button type="button" data-source-card="true" key={source.id} aria-pressed={wizard.source === source.id} onClick={() => void send({ type: "select_source", payload: source.id })} className={`flex min-h-[330px] flex-col items-start rounded-12 border p-6 text-left max-[1199px]:min-h-[220px] ${wizard.source === source.id ? "border-accent bg-accent/10" : "border-border bg-surface"} focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent`}><span aria-hidden="true" className="grid h-12 w-12 place-items-center rounded-8 bg-accent/10 text-18 text-accent">{source.icon}</span><span className="mt-4 block text-16 font-semibold text-text">{source.label}</span><span className="mt-2 block text-13 text-muted">{source.description}</span><span className="mt-auto pt-6 text-13 font-semibold text-accent">Choose →</span></button>)}</div><details className="text-13 text-muted"><summary className="w-fit cursor-pointer">More import options</summary><div className="mt-8 flex flex-wrap gap-8">{MORE_SOURCES.map((source) => <button type="button" key={source.id} aria-pressed={wizard.source === source.id} onClick={() => void send({ type: "select_source", payload: source.id })} className={`rounded-8 border px-12 py-8 text-text ${wizard.source === source.id ? "border-accent bg-accent/10" : "border-border bg-surface"}`}>{source.label}</button>)}</div><p className="mb-0 mt-8">Marketplace availability follows your network policy.</p></details><p className="m-0 text-12 text-muted">Tip: skills are folders with a SKILL.md. AgenTuna copies them into each agent’s skills folder for the scope you pick.</p></section>}
    {wizard.step === "details" && <section className="flex flex-col gap-12" aria-label="Source details">
      {activeImport && <div role="status" aria-live="polite" className="flex flex-wrap items-center gap-12 rounded-8 border border-accent/30 bg-accent/5 p-12 text-13 text-text"><span>{importProgress?.label ?? `Starting ${activeImport.source} import…`}</span><progress aria-label="Import progress" max={1} value={importProgress?.fraction ?? undefined} className="min-w-[120px] flex-1" /><button type="button" onClick={releaseActiveSkillImport} className="rounded-8 border border-border px-12 py-8 text-12 text-text">Cancel import</button></div>}
      {wizard.source === "new" && <section aria-label="Skill template" className="grid grid-cols-1 gap-8 sm:grid-cols-3">{([{ id: "blank", title: "Blank", note: "A clean starting point" }, { id: "checklist", title: "Checklist", note: "A sequence of checks" }, { id: "workflow", title: "Workflow", note: "Inputs, outputs and steps" }] as const).map(({ id, title, note }) => <button key={id} type="button" role="radio" aria-checked={wizard.details.template === id} onClick={() => setDetails({ template: id })} className={`rounded-8 border p-12 text-left ${wizard.details.template === id ? "border-accent bg-accent/10" : "border-border bg-surface"}`}><span className="block text-14 font-semibold text-text">{title}</span><span className="mt-4 block text-12 text-muted">{note}</span></button>)}</section>}
      {wizard.source === "git" && <><label className="flex flex-col gap-4 text-13 text-text">Repository URL<input aria-label="Repository URL" value={wizard.details.repo_url} onChange={(event) => setDetails({ repo_url: event.target.value, fetched_repo_selections: [] })} placeholder="https://host/owner/repository.git" className="rounded-8 border border-border bg-surface px-12 py-8" /></label><label className="flex flex-col gap-4 text-13 text-text">Branch, tag or commit (optional)<input value={wizard.details.repo_reference} onChange={(event) => setDetails({ repo_reference: event.target.value, fetched_repo_selections: [] })} className="rounded-8 border border-border bg-surface px-12 py-8" /></label><p className="m-0 text-12 text-muted">Only public HTTPS repositories are supported. AgenTuna does not request or store Git credentials.</p></>}
      {wizard.source === "git" && <section aria-label="Fetched repository skills" className="grid gap-12">
        <div className="flex flex-wrap items-center gap-8">
          <button type="button" disabled={busy || Boolean(activeImport) || !wizard.details.repo_url.trim()} onClick={() => void startSkillImport("git")} className="w-fit rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent">{activeImport?.source === "git" ? "Fetching…" : "Fetch skills"}</button>
          <button type="button" disabled={!importCandidates.some((candidate) => candidate.name && !candidate.error)} onClick={() => selectAllValidImportCandidates("git")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Select all</button>
          <button type="button" disabled={wizard.details.fetched_repo_selections.length === 0} onClick={() => clearImportSelection("git")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Select none</button>
        </div>
        {candidateError && <p role="alert" className="m-0 text-13 text-danger">{candidateError}</p>}
        {candidateError && /auth|credential|permission denied|private|401|403/i.test(candidateError) && <p className="m-0 text-12 text-muted">This repository needs authentication. AgenTuna imports public repositories only — open the project page and use its public clone URL; no credentials are requested or stored.</p>}
        {candidateStatus && <p role="status" className="m-0 text-12 text-muted">{candidateStatus}</p>}
        {importCandidates.length === 0 && !candidateError && !activeImport && <p role="status" className="m-0 text-12 text-muted">Fetch a public repository to see its available skills.</p>}
        <div className="grid min-w-0 gap-12 lg:grid-cols-2">
          <div data-testid="git-import-candidate-list" ref={candidateListRef} onScroll={(event) => { candidateScrollTop.current = event.currentTarget.scrollTop; }} className="grid min-h-[320px] max-h-[420px] content-start gap-8 overflow-auto rounded-8 border border-border p-8">
            {importCandidates.map((candidate) => {
              const disabledReason = candidate.error ?? (!candidate.name ? "This folder does not contain a valid SKILL.md." : null);
              const selected = wizard.details.fetched_repo_selections.includes(candidate.id);
              return <label key={candidate.id} title={disabledReason ?? undefined} className={`flex items-start gap-8 rounded-8 border p-12 text-13 text-text ${selected ? "border-accent bg-accent/10" : "border-border"}`}>
                <input type="checkbox" disabled={Boolean(disabledReason)} checked={selected} onFocus={() => setPreviewedCandidateId(candidate.id)} onChange={(event) => toggleImportCandidate("git", candidate.id, event.target.checked)} />
                <span className="grid gap-4"><span className="flex items-center gap-4"><strong>{candidate.name ?? "Invalid skill"}</strong><span className={`rounded-full px-6 py-2 text-11 ${disabledReason ? "bg-danger/10 text-danger" : "bg-success/10 text-success"}`}>{disabledReason ? "Invalid" : "Valid"}</span></span><span className="text-12 text-muted">{candidate.error ?? candidate.description ?? candidate.id}</span></span>
              </label>;
            })}
          </div>
          {previewCandidate(previewedCandidate) ?? <p className="m-0 self-start rounded-8 border border-border p-12 text-12 text-muted">Select a valid skill to preview its SKILL.md.</p>}
        </div>
      </section>}
      {wizard.source === "library" && (() => {
        const selectedSource = libraryCatalog?.sources.find((source) => source.id === librarySourceId) ?? null;
        const sourceSkills = libraryCatalog?.skills.filter((skill) => skill.source_id === librarySourceId) ?? [];
        const emptyLibrary = libraryCatalog !== null && libraryCatalog.sources.length === 0;
        return <section aria-label="Library import" className="grid gap-12 rounded-10 border border-border bg-surface p-12 lg:grid-cols-[240px_minmax(0,1fr)]">
          <div className="grid content-start gap-4">
            <h3 className="mb-2 mt-0 text-11 font-semibold uppercase tracking-wide text-text-muted">Sources</h3>
            {libraryCatalogError && <p role="alert" className="m-0 text-13 text-danger">{libraryCatalogError}</p>}
            {!libraryCatalog && !libraryCatalogError && <p role="status" className="m-0 text-13 text-text-muted">Loading skills from other scopes and projects…</p>}
            {emptyLibrary && <div role="status" className="grid gap-8 rounded-8 border border-border p-12 text-13 text-text-secondary">
              <p className="m-0">No skills found in <code className="break-all font-mono text-12 text-text">{libraryCatalog.search_root}</code>. Choose a folder…</p>
              <button type="button" onClick={() => chooseSource("folder")} className="w-fit rounded-8 border border-border px-12 py-8 text-12 text-text">Choose a folder…</button>
            </div>}
            {libraryCatalog?.sources.map((source) => <button type="button" key={source.id} aria-pressed={source.id === librarySourceId} onClick={() => setLibrarySourceId(source.id)} className={`flex items-center justify-between gap-8 rounded-8 border px-12 py-8 text-left text-13 ${source.id === librarySourceId ? "border-border bg-raised font-medium text-text" : "border-transparent text-text-secondary hover:bg-overlay"}`}><span className="min-w-0 truncate">{source.label}</span><span className="shrink-0 font-mono text-12 text-text-muted">{source.count}</span></button>)}
            {libraryCatalog && libraryCatalog.skipped.length > 0 && <div className="mt-4 grid gap-4 rounded-8 border border-warning/30 bg-warning/10 p-8 text-11 text-warning">{libraryCatalog.skipped.map((skip) => <p key={skip.path} className="m-0"><strong>{skip.label}</strong>: {skip.reason}</p>)}</div>}
          </div>
          <div className="grid min-w-0 content-start gap-12">
            {selectedSource === null ? <p className="m-0 text-12 text-text-muted">Choose a source to list its skills.</p> : <>
              <p className="m-0 truncate rounded-6 bg-overlay px-12 py-8 font-mono text-11 text-text-secondary" title={selectedSource.id}>{selectedSource.id}</p>
              <div className="flex items-center justify-between gap-8"><span className="text-11 uppercase tracking-wide text-text-muted">{sourceSkills.length} skill{sourceSkills.length === 1 ? "" : "s"}</span><button type="button" disabled={!sourceSkills.some((skill) => !skill.error && !skill.clash_reason)} onClick={selectAllAvailableLibrarySkills} className="rounded-6 border border-border px-12 py-8 text-11 text-text">Select all</button></div>
              <ul aria-label="Skills in selected source" className="m-0 flex max-h-[360px] flex-col gap-4 overflow-auto p-0">
                {sourceSkills.map((skill) => {
                  const alreadyInRepo = skill.clash_reason !== null;
                  const disabledReason = skill.error ?? skill.clash_reason;
                  const selected = wizard.details.library_selections.includes(skill.id);
                  return <li key={skill.id} className={`list-none rounded-8 border p-12 ${selected ? "border-accent bg-accent/10" : "border-border"}`}>
                    <label title={disabledReason ?? undefined} className="flex items-start gap-8 text-13 text-text">
                      <input type="checkbox" disabled={Boolean(disabledReason)} checked={selected} onChange={(event) => toggleLibrarySkill(skill.id, event.target.checked)} />
                      <span className="grid min-w-0 gap-4"><span className="flex flex-wrap items-center gap-8"><strong>{skill.name ?? "Invalid skill"}</strong>{alreadyInRepo && <span className="inline-flex items-center gap-4 rounded-4 bg-warning/15 px-6 py-2 text-11 text-warning"><i aria-hidden="true" className="size-4 rounded-full bg-warning" />Already in this repo</span>}</span><span className="text-12 text-text-muted">{skill.description ?? skill.path}</span>{disabledReason && !alreadyInRepo && <span className="text-11 text-danger">{disabledReason}</span>}</span>
                    </label>
                  </li>;
                })}
              </ul>
              <section aria-label="Selected library skills" className="grid gap-4 rounded-8 bg-overlay px-12 py-8 text-12 text-text-muted">
                <strong className="font-medium text-text">Selected ({selectedLibrarySkills.length})</strong>
                {selectedLibrarySkills.length ? selectedLibrarySkills.map((skill) => <span key={skill.id}>{skill.name ?? skill.id} · {skill.source_label}</span>) : <span>No skills selected</span>}
              </section>
              <fieldset className="grid gap-8 rounded-8 border border-border p-12">
                <legend className="px-4 text-13 font-medium text-text">How to add them</legend>
                <div role="radiogroup" aria-label="Library import behavior" className="grid grid-cols-2 gap-4 rounded-6 bg-overlay p-4">
                  <button type="button" role="radio" aria-checked={libraryTransferMode === "copy"} onClick={() => setDetails({ library_mode: "copy" })} className={`rounded-6 px-12 py-8 text-12 font-medium ${libraryTransferMode === "copy" ? "bg-raised text-text" : "text-text-secondary"}`}>Copy</button>
                  <button type="button" role="radio" aria-checked={libraryTransferMode === "link"} onClick={() => setDetails({ library_mode: "link" })} className={`rounded-6 px-12 py-8 text-12 font-medium ${libraryTransferMode === "link" ? "bg-raised text-text" : "text-text-secondary"}`}>Link <span className="font-normal text-text-muted">(stays in sync)</span></button>
                </div>
                <p className="m-0 px-4 text-11 text-text-muted">Copy makes an independent copy. Link points at the original folder, so edits there show up here.</p>
              </fieldset>
            </>}
          </div>
        </section>;
      })()}
      {wizard.source === "folder" && <div className="flex items-end gap-8"><label className="flex flex-1 flex-col gap-4 text-13 text-text">Folder path<input aria-label="Folder path" value={wizard.details.folder_path} onChange={(event) => setDetails({ folder_path: event.target.value, folder_selections: [] })} placeholder="Choose a folder" className="rounded-8 border border-border bg-surface px-12 py-8" /></label><button type="button" disabled={busy} onClick={() => void chooseImportPath("folder")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Browse…</button><button type="button" disabled={busy || Boolean(activeImport) || !wizard.details.folder_path} onClick={() => void startSkillImport("folder")} className="rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent">{activeImport?.source === "folder" ? "Scanning…" : "Scan folder"}</button></div>}
      {wizard.source === "zip" && <div className="flex items-end gap-8"><label className="flex flex-1 flex-col gap-4 text-13 text-text">ZIP archive path<input aria-label="ZIP path" value={wizard.details.zip_path} onChange={(event) => setDetails({ zip_path: event.target.value, zip_selections: [] })} placeholder="Choose a .zip file" className="rounded-8 border border-border bg-surface px-12 py-8" /></label><button type="button" disabled={busy} onClick={() => void chooseImportPath("zip")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Browse…</button><button type="button" disabled={busy || Boolean(activeImport) || !wizard.details.zip_path} onClick={() => void startSkillImport("zip")} className="rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent">{activeImport?.source === "zip" ? "Scanning…" : "Scan ZIP"}</button></div>}
      {(wizard.source === "folder" || wizard.source === "zip") && <section aria-label="Detected skills" className="grid gap-12">
        <div className="flex flex-wrap items-center gap-8">
          <button type="button" disabled={!importCandidates.some((candidate) => candidate.name && !candidate.error)} onClick={() => selectAllValidImportCandidates(wizard.source as "folder" | "zip")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Select all</button>
          <button type="button" disabled={wizard.source === "folder" ? wizard.details.folder_selections.length === 0 : wizard.details.zip_selections.length === 0} onClick={() => clearImportSelection(wizard.source as "folder" | "zip")} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Select none</button>
        </div>
        {candidateError && <p role="alert" className="m-0 text-13 text-danger">{candidateError}</p>}
        {candidateStatus && <p role="status" className="m-0 text-12 text-muted">{candidateStatus}</p>}
        {importCandidates.length === 0 && !candidateError && !activeImport && <p role="status" className="m-0 text-12 text-muted">Scan a folder or ZIP file to find SKILL.md files.</p>}
        <div className="grid min-w-0 gap-12 lg:grid-cols-2">
          <div ref={candidateListRef} onScroll={(event) => { candidateScrollTop.current = event.currentTarget.scrollTop; }} className="grid min-h-[320px] max-h-[420px] content-start gap-8 overflow-auto rounded-8 border border-border p-8">
            {importCandidates.map((candidate) => {
              const source = wizard.source === "folder" ? "folder" : "zip";
              const disabledReason = candidate.error ?? (!candidate.name ? "This folder does not contain a valid SKILL.md." : null);
              const selected = selectedImportIds(source).includes(candidate.id);
              return <label key={candidate.id} title={disabledReason ?? undefined} className={`flex items-start gap-8 rounded-8 border p-12 text-13 text-text ${selected ? "border-accent bg-accent/10" : "border-border"}`}>
                <input type="checkbox" disabled={Boolean(disabledReason)} checked={selected} onFocus={() => setPreviewedCandidateId(candidate.id)} onChange={(event) => toggleImportCandidate(source, candidate.id, event.target.checked)} />
                <span className="grid gap-4"><span className="flex items-center gap-4"><strong>{candidate.name ?? "Invalid skill"}</strong><span className={`rounded-full px-6 py-2 text-11 ${disabledReason ? "bg-danger/10 text-danger" : "bg-success/10 text-success"}`}>{disabledReason ? "Invalid" : "Valid"}</span></span><span className="text-12 text-muted">{candidate.description ?? candidate.error ?? candidate.id}</span></span>
              </label>;
            })}
          </div>
          {previewCandidate(previewedCandidate) ?? <p className="m-0 self-start rounded-8 border border-border p-12 text-12 text-muted">Select a valid skill to preview its SKILL.md.</p>}
        </div>
      </section>}
      {requiresName && <label className="flex flex-col gap-4 text-13 text-text">Skill name<input aria-label="Skill name" value={wizard.name} onChange={(event) => void send({ type: "set_name", payload: event.target.value })} placeholder="Name your skill" className="rounded-8 border border-border bg-surface px-12 py-8" /></label>}
      {wizard.source === "marketplace" && <div className="grid gap-12 rounded-8 border border-border bg-surface p-12"><p role="status" className="m-0 text-13 text-muted">{marketplacePolicy ? marketplacePolicy.marketplace_enabled ? "Marketplace search is enabled. Search runs only after you press Search." : marketplacePolicy.offline ? "Offline policy is active; cached marketplace results may be available." : "Marketplace browsing is unavailable because network policy disables it." : "Checking marketplace network policy…"}</p>{marketplacePolicy && (marketplacePolicy.marketplace_enabled || marketplacePolicy.offline) && <div className="flex gap-8"><input aria-label="Search marketplace" value={marketplaceQuery} onChange={(event) => setMarketplaceQuery(event.target.value)} placeholder="Search skills.sh" className="min-w-0 flex-1 rounded-8 border border-border bg-surface px-12 py-8" /><button type="button" disabled={busy || !marketplaceQuery.trim()} onClick={() => void searchMarketplace(false)} className="rounded-8 bg-accent-fill px-12 py-8 text-13 text-on-accent">Search</button><button type="button" disabled={busy || !marketplaceQuery.trim()} onClick={() => void searchMarketplace(true)} className="rounded-8 border border-border px-12 py-8 text-13 text-text">Refresh</button></div>}{marketplaceError && <p role="alert" className="m-0 text-13 text-danger">{marketplaceError}</p>}{marketplaceResults && <><p className="m-0 text-12 text-muted">{marketplaceResults.provenance}{marketplaceResults.offline ? " · cached/offline" : ""}{marketplaceResults.stale ? " · stale" : ""}</p>{marketplaceResults.results.map((result) => <button key={result.id} type="button" aria-pressed={wizard.details.marketplace_id === result.id} onClick={() => { setDetails({ marketplace_id: result.id, repo_url: result.source }); void send({ type: "set_name", payload: result.name }); }} className={`grid gap-4 rounded-8 border p-12 text-left ${wizard.details.marketplace_id === result.id ? "border-accent bg-accent/10" : "border-border"}`}><span className="text-14 font-semibold text-text">{result.name}</span><span className="text-12 text-muted">{result.author} · {formatInstalls(result.installs)} installs</span><span className="break-all text-12 text-muted">Source: {result.source}</span><span className="text-12 text-muted">{result.description}</span></button>)}</>}</div>}
      {wizard.source === "new" && <><label className="flex flex-col gap-4 text-13 text-text">Use when<textarea aria-label="Use when description" maxLength={1024} value={wizard.details.description} onChange={(event) => setDetails({ description: event.target.value })} className="min-h-80 rounded-8 border border-border bg-surface px-12 py-8" /><span aria-live="polite" className={`text-12 ${wizard.details.description.length >= 1024 ? "text-danger" : "text-muted"}`}>{wizard.details.description.length}/1024</span></label><div className="grid gap-12 lg:grid-cols-2"><section className="rounded-8 border border-border p-12"><h3 className="m-0 mb-8 text-13 font-semibold text-text">Preview</h3>{wizard.name.trim() ? <pre aria-label="Generated SKILL.md preview" className="m-0 max-h-[400px] overflow-auto whitespace-pre-wrap text-12 text-text">{templatePreview?.markdown ?? "Generating preview…"}</pre> : <p className="m-0 text-12 text-muted">Enter a name to preview the generated SKILL.md.</p>}</section><div className="self-start rounded-8 border border-border p-12 text-12 text-muted">Slug: {templatePreview?.slug ?? "—"}{templatePreview?.name_error && <p role="alert" className="text-danger">{templatePreview.name_error}</p>}{templatePreview?.description_error && <p role="alert" className="text-danger">{templatePreview.description_error}</p>}</div></div></>}
    </section>}
    {wizard.step === "scope_agents" && <section className="flex flex-col gap-12" aria-label="Scope & agents">
      {scopeOptionsError && <p role="alert" className="m-0 rounded-8 border border-danger/30 bg-danger/10 p-12 text-12 text-danger">{scopeOptionsError}</p>}
      <section aria-label="Destination scope" className="grid gap-8">
        <h3 className="m-0 text-13 font-semibold text-text">Scope</h3>
        <div role="radiogroup" aria-label="Skill destination scope" className="grid grid-cols-1 gap-12 md:grid-cols-3">
          {scopeOptions?.scopes.filter((card) => card.scope !== "local" || localScopeExpanded || wizard.scope === "local").map((card) => {
            const selected = wizard.scope === card.scope;
            const label = card.scope === "global" ? "Global" : card.scope === "user" ? "User" : card.scope === "project" ? "Project" : "Local";
            return <button key={card.scope} type="button" role="radio" aria-checked={selected} disabled={!card.available || busy} onClick={() => void send({ type: "set_scope", payload: card.scope })} className={`grid min-h-[122px] content-start gap-7 rounded-10 border p-12 text-left ${selected ? "border-accent bg-accent/10" : "border-border bg-surface"} disabled:cursor-not-allowed disabled:opacity-55`}>
              <span className="flex items-center justify-between gap-8"><strong className="text-14 text-text">{label}</strong>{selected && <span className="text-11 font-semibold text-accent">Selected</span>}</span>
              <span className="text-12 text-muted">{card.note}</span>
              {card.store_path && <code className="break-all text-11 text-text-secondary">{card.store_path}</code>}
              {card.disabled_reason && <span className="text-11 text-danger">{card.disabled_reason}</span>}
              {card.lock_note && <span role="note" className="text-11 text-warning">{card.lock_note}</span>}
            </button>;
          })}
        </div>
        <button type="button" aria-expanded={localScopeExpanded || wizard.scope === "local"} onClick={() => setLocalScopeExpanded((expanded) => !expanded)} className="w-fit rounded-6 px-8 py-8 text-12 text-accent hover:bg-overlay focus-visible:outline-2 focus-visible:outline-focus">{localScopeExpanded || wizard.scope === "local" ? "Hide Local scope" : "Show Local scope"}</button>
      </section>
      <section aria-label="Destination agents" className="grid gap-8">
        <h3 className="m-0 text-13 font-semibold text-text">Agents</h3>
        {scopeOptionsBusy && <p role="status" className="m-0 text-12 text-muted">Resolving agent paths and name clashes…</p>}
        <fieldset className="m-0 grid gap-8 rounded-10 border border-border bg-raised p-12">
          <legend className="sr-only">Agents that will load this skill</legend>
          {scopeOptions?.agents.map((agent) => {
            const checked = wizard.agents.includes(agent.agent);
            const disabled = agent.disabled_reason !== null;
            const targetPaths = agent.targets.filter((target) => selectedSkillNames.includes(target.name));
            return <div key={agent.agent} className="grid gap-4 border-b border-border py-8 last:border-0">
              <div className="flex flex-wrap items-start justify-between gap-8">
                <label className={`flex min-w-0 items-start gap-8 text-13 ${disabled ? "text-text-muted" : "text-text"}`}>
                  <input type="checkbox" disabled={disabled || busy} checked={checked} onChange={(event) => {
                    const agents = event.target.checked ? [...wizard.agents, agent.agent] : wizard.agents.filter((entry) => entry !== agent.agent);
                    void send({ type: "set_agents", payload: agents });
                  }} />
                  <span className="grid gap-4"><strong>{agent.name}</strong>{agent.disabled_reason && <span className="text-11 text-danger">{agent.disabled_reason}</span>}</span>
                </label>
                {disabled && <button type="button" disabled={agentPathLoading} onClick={() => void setAgentPath(agent.agent)} className="rounded-6 border border-border px-8 py-8 text-11 text-text hover:bg-overlay disabled:opacity-50">{agentPathLoading ? "Loading…" : "Set path"}</button>}
              </div>
              {agent.skills_dir && <code className="break-all pl-24 text-11 text-text-secondary">Skills path: {agent.skills_dir}</code>}
              {checked && targetPaths.map((target) => <code key={`${agent.agent}:${target.name}`} className="break-all pl-24 text-11 text-text-secondary">{target.name}: {target.path}</code>)}
              {checked && agent.shared_with.length > 0 && <p className="m-0 pl-24 text-11 text-warning">Shared skills folder with {agent.shared_with.join(", ")}; enabling this path loads the skill in both agents.</p>}
            </div>;
          })}
          {scopeOptions && scopeOptions.agents.length === 0 && <p className="m-0 text-12 text-muted">No supported agents are available.</p>}
        </fieldset>
      </section>
      {scopeOptions?.clashes.length ? <section aria-label="Skill name checks" className="grid gap-8">
        {scopeOptions.clashes.map((clash) => <div key={clash.name} role={clash.blocking.length ? "alert" : "status"} className={`grid gap-4 rounded-8 border p-12 text-12 ${clash.blocking.length ? "border-danger/30 bg-danger/10 text-danger" : clash.warnings.length ? "border-warning/30 bg-warning/10 text-warning" : "border-success/30 bg-success/10 text-success"}`}>
          <strong>{clash.blocking.length ? "Name clash" : clash.warnings.length ? "Existing copies" : "No name clash"} · {clash.name}</strong>
          {clash.blocking.map((message) => <span key={message}>{message}</span>)}
          {clash.warnings.map((message) => <span key={message}>{message}</span>)}
        </div>)}
      </section> : scopeOptions && selectedSkillNames.length > 0 && wizard.scope && <p role="status" className="m-0 rounded-8 border border-success/30 bg-success/10 p-12 text-12 text-success">No name clash for {selectedSkillNames.join(", ")} in {wizard.scope}.</p>}
    </section>}
    {wizard.step === "review" && activeApplyJob && <div role="status" aria-live="polite" className="flex flex-wrap items-center gap-12 rounded-8 border border-accent/30 bg-accent/5 p-12 text-13 text-text"><span>{applyProgress?.label ?? "Creating skill…"}</span><progress aria-label="Skill creation progress" max={1} value={applyProgress?.fraction ?? undefined} className="min-w-[120px] flex-1" /></div>}
    {wizard.step === "review" && <section className="flex flex-col gap-12" aria-label="Review">
      <div className="grid grid-cols-1 items-start gap-12 lg:grid-cols-2">
        <section aria-label="Summary" className="grid content-start gap-12 rounded-10 border border-border bg-surface p-16">
          <h3 className="m-0 text-14 font-semibold text-text">Summary</h3>
          <dl className="grid gap-12 text-13">
            <div className="grid gap-4"><dt className="text-11 uppercase text-muted">Name</dt><dd className="m-0 text-text">{wizard.name || preview?.items.map((item) => item.name).join(", ") || sourceSelection}</dd></div>
            <div className="grid gap-4"><dt className="text-11 uppercase text-muted">Source</dt><dd className="m-0 text-text">{labelForSource}</dd></div>
            <div className="grid gap-4"><dt className="text-11 uppercase text-muted">Source selection</dt><dd className="m-0 text-text">{sourceSelection}</dd></div>
            <div className="grid gap-4"><dt className="text-11 uppercase text-muted">Scope</dt><dd className="m-0 text-text">{wizard.scope}</dd></div>
            <div className="grid gap-4"><dt className="text-11 uppercase text-muted">Agents</dt><dd className="m-0 text-text">{wizard.agents.join(", ")}</dd></div>
          </dl>
          {wizard.scope === "local" && <p className="m-0 rounded-8 border border-warning/30 bg-warning/10 p-12 text-12 text-warning"><strong>Local</strong> — only you in this repo; its files are not committed to Git.</p>}
        </section>
        {preview ? <section aria-label="Where files go" className="grid content-start gap-12 rounded-10 border border-border bg-surface p-16">
          <h3 className="m-0 text-14 font-semibold text-text">Where files go</h3>
          {preview.store_init && <p aria-label="Store creation" className="m-0 flex flex-wrap items-baseline gap-4 rounded-8 border border-border bg-surface p-8 text-12 text-text" data-slot="skill-store-init"><strong>Create store at</strong><code className="break-all">{preview.store_init.path}</code><span className="text-muted">({preview.store_init.files_created} new entries, reviewed with this import)</span></p>}
          {preview.files.length > 0 && <ul aria-label="Planned file paths" className="m-0 grid gap-4 p-0 text-12 text-text">{preview.files.map((file) => <li key={`${file.path}:${file.package_path}`} className="list-inside list-disc break-all">{file.path}{file.diff && <details className="ml-16"><summary className="cursor-pointer">Review existing-file diff</summary><pre className="overflow-auto whitespace-pre-wrap">{file.diff}</pre></details>}</li>)}</ul>}
          {preview.deployments.length > 0 && <ul aria-label="Planned deployments" className="m-0 grid gap-4 p-0 text-12 text-text">{preview.deployments.map((deployment) => <li key={deployment.agent} className="list-inside list-disc break-all">{deployment.agent}: {deployment.mode} → {deployment.path}</li>)}</ul>}
          {preview.items.length > 0 && <ul aria-label="Reviewed skill package transfers" className="m-0 grid gap-4 rounded-8 border border-border p-12 text-12 text-text">{preview.items.map((item) => <li key={`${item.name}:${item.target_path}`} className="grid gap-4 break-all"><strong>{item.mode === "link" ? "Link" : "Copy"} · {item.name}</strong><span>{item.source_path ?? "New skill"} → {item.target_path}</span></li>)}</ul>}
          {preview.stage_available && <label className="flex items-center gap-8 text-13 text-text"><input type="checkbox" checked={stageRequested} onChange={(event) => { const stage = event.target.checked; setStageRequested(stage); void refreshPreview(stage); }} />Stage these project-scope paths with Git</label>}
          {preview.requires_overwrite_confirmation && <label className="flex items-start gap-8 text-13 text-text"><input type="checkbox" checked={confirmOverwrites} onChange={(event) => setConfirmOverwrites(event.target.checked)} />I reviewed the diffs and confirm replacing these existing files.</label>}
          <div className="grid gap-4 border-t border-border pt-10">
            <span className="text-11 uppercase text-muted">AgenTuna store</span>
            <code className="break-all text-12 text-text">{preview.canonical_dir}</code>
            <span className="text-11 text-muted">Paths and deployment modes match the reviewed plan.</span>
          </div>
        </section> : <p role="status" className="m-0 text-12 text-muted">Preparing the exact file plan…</p>}
      </div>
    </section>}
    {wizard.step === "done" && outcome && <section role="status" className="mx-auto flex w-full max-w-[720px] flex-col items-center gap-16 py-24 text-center">
      <span aria-hidden="true" className="grid size-[84px] place-items-center rounded-full bg-success/15 text-success"><svg viewBox="0 0 24 24" className="h-10 w-10" fill="none" stroke="currentColor" strokeWidth="2.5"><path d="m5 12 4 4L19 6" /></svg></span>
      <div className="grid gap-4">
        <h2 className="m-0 text-26 font-semibold text-text">{outcome.skill_names.length === 1 ? `${outcome.skill_names[0]} is ready` : `${outcome.skill_names.length} skills are ready`}</h2>
        <p className="m-0 text-13 text-text-muted">{doneAgents.length > 0 ? `${doneAgents.join(" and ")} ${doneAgents.length === 1 ? "loads" : "load"} ${outcome.skill_names.length === 1 ? "it" : "them"} in their next session${wizard.scope === "project" ? " in this repo" : wizard.scope === "user" ? " in every repo" : ""}.` : "Saved in the AgenTuna store; no agent deployment was requested."}</p>
      </div>
      <div className="grid w-full gap-8 rounded-12 border border-border bg-surface p-16 text-left text-12 text-text">
        {outcome.paths_written.length > 0 && <ul aria-label="Written file paths" className="m-0 grid gap-4 p-0">{outcome.paths_written.map((path) => <li key={path} className="flex items-start gap-8 break-all"><span aria-hidden="true" className="text-success">✓</span>Wrote {path}</li>)}</ul>}
        {outcome.deployments.length > 0 && <ul aria-label="Actual deployments" className="m-0 grid gap-4 p-0">{outcome.deployments.map((deployment) => <li key={deployment.agent} className="flex items-start gap-8 break-all"><span aria-hidden="true" className="text-success">✓</span>{AGENT_LABELS[deployment.agent] ?? deployment.agent} ({deployment.mode}): {deployment.path}</li>)}</ul>}
        <p className="m-0 flex items-start gap-8 text-text-muted"><span aria-hidden="true" className="text-success">✓</span>{outcome.backup_hint}</p>
        <details className="text-11 text-text-muted"><summary className="cursor-pointer">Details</summary><p className="m-0 break-all">Operation: {outcome.operation_id}</p><p className="m-0">{outcome.staged ? `Git staging: staged ${outcome.staged_paths.join(", ")}` : "Git staging: not staged"}</p>{outcome.stage_error && <p role="alert" className="m-0 text-danger">{outcome.stage_error}</p>}</details>
      </div>
    </section>}
    {wizard.step === "done" ? <footer className="flex justify-end gap-8 border-t border-border pt-14"><button type="button" onClick={() => { const fresh = emptyForScope(initialScope); wizardRef.current = fresh; setWizard(fresh); setError(null); setPreview(null); setOutcome(null); setStageRequested(false); setConfirmOverwrites(false); }} className="rounded-8 border border-border px-14 py-8 text-13 text-text">Add another</button><button type="button" onClick={onClose} className="rounded-8 bg-accent-fill px-14 py-8 text-13 font-medium text-on-accent">Back to Skills</button></footer> : <footer className="flex items-center justify-between border-t border-border pt-14"><button type="button" disabled={wizard.step === "source" || busy} onClick={() => void send({ type: "back" })} className="rounded-8 border border-border px-14 py-8 text-13 text-text disabled:opacity-50">Back</button><div className="flex gap-8"><button type="button" disabled={busy} onClick={() => void send({ type: "cancel" })} className="rounded-8 border border-border px-14 py-8 text-13 text-text disabled:opacity-50">Cancel</button><button type="button" data-wizard-next="true" disabled={busy || !canAdvance} onClick={() => void send({ type: "next" })} className="rounded-8 bg-accent-fill px-14 py-8 text-13 font-medium text-on-accent disabled:opacity-50">{wizard.step === "review" ? "Create skill" : wizard.source === "library" && selectedLibrarySkills.length > 0 ? `Continue with ${selectedLibrarySkills.length}` : "Continue"}</button></div></footer>}
    {discardConfirm && <div role="alertdialog" aria-modal="true" aria-labelledby="discard-title" className="fixed inset-0 z-50 grid place-items-center bg-black/40"><div className="w-full max-w-[380px] rounded-12 border border-border bg-surface p-16"><h2 id="discard-title" className="m-0 text-18 font-semibold text-text">Discard this skill draft?</h2><p className="text-13 text-muted">Your entered details will be lost. No files have been written.</p><div className="flex justify-end gap-8"><button type="button" onClick={() => setDiscardConfirm(false)} className="rounded-8 border border-border px-12 py-8 text-13">Keep editing</button><button type="button" onClick={() => { setDiscardConfirm(false); void send({ type: "confirm_discard" }); }} className="rounded-8 bg-danger px-12 py-8 text-13 text-white">Discard</button></div></div></div>}
    <ConnectAgentDialog agent={pathAgent} onClose={() => setPathAgent(null)} onSaved={() => { setScopeRefreshVersion((version) => version + 1); }} />
  </main>;
}
