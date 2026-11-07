/**
 * New rule file dialog (TA-057).
 *
 * Modal over `at_engine::rule_editor::new_rule` via the typed
 * `instruction_create_*` commands. The dialog shows file-type radio cards
 * (CLAUDE.md, AGENTS.md, CLAUDE.local.md), scope radio cards, a read-only target
 * path preview and a Create action. Changing type or scope re-runs the preview so
 * the path is always the adapter table's answer. An existing target shows an
 * inline conflict and Create stays disabled; Global is disabled with its
 * read-only reason and Local is disabled when the project is not a Git repo.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, RadioCardGroup } from "../../components";
import { Modal } from "../dialogs/Modal";
import { useToast } from "../toasts";
import type { InstructionCreateOptions } from "../../generated/InstructionCreateOptions";
import type { InstructionCreatePreview } from "../../generated/InstructionCreatePreview";

/** Native rule filenames the dialog can create. */
const FILE_TYPES = ["CLAUDE.md", "AGENTS.md", "CLAUDE.local.md"] as const;
type RuleFileName = (typeof FILE_TYPES)[number];

/** Scopes the dialog offers, in precedence order. */
const SCOPES = ["global", "user", "project", "local"] as const;
type RuleScope = (typeof SCOPES)[number];

const SCOPE_DESCRIPTIONS: Readonly<Record<RuleScope, string>> = {
  global: "Every repository on this Mac",
  user: "You, in every repository",
  project: "This repository, committed to Git",
  local: "Only you, this repository",
};

const SCOPE_DOT_CLASSES: Readonly<Record<RuleScope, string>> = {
  global: "bg-scope-global",
  user: "bg-scope-user",
  project: "bg-scope-project",
  local: "bg-scope-local",
};

export interface NewRuleDialogProps {
  /** Whether the dialog is open. */
  readonly open: boolean;
  /** Active project id, when one is selected. */
  readonly project: string | null;
  /** Active scope selected by the Add menu. */
  readonly initialScope?: RuleScope;
  /** Called after a close with nothing written. */
  readonly onClose: () => void;
  /** Called with the created absolute path after a successful create. */
  readonly onCreated: (path: string, fileName: string) => void | Promise<void>;
}

/**
 * Renders the new rule file dialog. All command failures surface inline or as a
 * warning toast; Escape and Cancel write nothing.
 */
export function NewRuleDialog({ open, project, initialScope, onClose, onCreated }: NewRuleDialogProps) {
  const toast = useToast();
  const [fileType, setFileType] = useState<RuleFileName>("CLAUDE.md");
  const [scope, setScope] = useState<RuleScope>(initialScope ?? (project === null ? "user" : "project"));
  const [options, setOptions] = useState<InstructionCreateOptions | null>(null);
  const [preview, setPreview] = useState<InstructionCreatePreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [createBusy, setCreateBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const firstCardRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) return;
    setScope(initialScope ?? (project === null ? "user" : "project"));
    setError(null);
    let cancelled = false;
    invoke<InstructionCreateOptions>("instruction_create_options", {
      projectId: project,
    })
      .then((result) => {
        if (!cancelled) setOptions(result);
      })
      .catch((cause) => {
        if (!cancelled) setError(String(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [open, project, initialScope]);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPreviewBusy(true);
    invoke<InstructionCreatePreview>("instruction_create_preview", {
      fileType,
      scope,
      projectId: project,
    })
      .then((result) => {
        if (!cancelled) {
          setPreview(result);
          setError(null);
        }
      })
      .catch((cause) => {
        if (!cancelled) setError(String(cause));
      })
      .finally(() => {
        if (!cancelled) setPreviewBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, fileType, scope, project]);

  useEffect(() => {
    if (open) firstCardRef.current?.focus();
  }, [open]);

  const scopeDisabledReason = useCallback(
    (candidate: RuleScope): string | null => {
      if (candidate === "global" && options?.global_writable === false) {
        return options.global_reason ?? "Global is read-only";
      }
      if (candidate === "local" && options?.local_available === false) {
        return options.local_reason ?? "Local needs a Git work tree";
      }
      if (
        (candidate === "project" || candidate === "local") &&
        options?.has_project === false
      ) {
        return "Select a project first";
      }
      return null;
    },
    [options],
  );

  const conflict = preview?.exists === true;
  const blockedReason =
    preview?.reason !== undefined && preview.reason !== null && !conflict
      ? preview.reason
      : null;
  const canCreate =
    !createBusy && !previewBusy && !conflict && blockedReason === null && preview?.path != null;

  async function create(): Promise<void> {
    if (!canCreate) return;
    setCreateBusy(true);
    setError(null);
    try {
      const created = await invoke<string>("instruction_create", {
        request: { file_type: fileType, scope },
        projectId: project,
      });
      onClose();
      toast.success(`${fileType} created`, { title: created });
      await onCreated(created, fileType);
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Rule file not created" });
    } finally {
      setCreateBusy(false);
    }
  }

  return (
    <Modal
      open={open}
      title="New rule file"
      description="AgenTuna creates the file at the agent's native path and never overwrites an existing one."
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!canCreate}
            onClick={() => void create()}
            aria-label={`Create ${fileType} in ${scope} scope`}
          >
            {createBusy ? "Creating…" : "Create"}
          </Button>
        </>
      }
    >
      <div className="grid gap-16">
        <RadioCardGroup<RuleFileName>
          label="File type"
          value={fileType}
          onValueChange={setFileType}
          columns={3}
          options={FILE_TYPES.map((name) => ({
            value: name,
            title: name,
            description:
              name === "CLAUDE.local.md"
                ? "Personal, never committed"
                : name === "AGENTS.md"
                  ? "Codex and omp"
                  : "Claude Code",
          }))}
        />
        <RadioCardGroup<RuleScope>
          label="Scope"
          value={scope}
          onValueChange={setScope}
          columns={2}
          options={SCOPES.map((candidate) => {
            const reason = scopeDisabledReason(candidate);
            return {
              value: candidate,
              title: candidate[0]?.toUpperCase() + candidate.slice(1),
              description: SCOPE_DESCRIPTIONS[candidate],
              badge: (
                <span className="inline-flex items-center gap-4 text-11 text-text-secondary">
                  <span aria-hidden="true" className={`size-8 rounded-full ${SCOPE_DOT_CLASSES[candidate]}`} />
                  {candidate[0]?.toUpperCase() + candidate.slice(1)}
                </span>
              ),
              disabled: reason !== null,
            };
          })}
        />
        {options !== null && !options.global_writable && (
          <p className="m-0 text-12 text-text-muted" role="note">
            Global is unavailable: {options.global_reason ?? "read-only"}
          </p>
        )}
        {options !== null && !options.local_available && (
          <p className="m-0 text-12 text-text-muted" role="note">
            Local is unavailable: {options.local_reason ?? "not a Git repository"}
          </p>
        )}
        <div className="rounded-8 border border-border bg-raised p-12" aria-live="polite">
          <p className="m-0 text-11 uppercase tracking-wide text-text-muted">Target path</p>
          <p className="m-0 mt-4 break-all font-mono text-12 text-text">
            {previewBusy
              ? "Resolving…"
              : (preview?.path ?? "No native path for this file type and scope")}
          </p>
          {conflict && preview?.path != null && (
            <p role="alert" className="m-0 mt-8 text-12 text-danger">
              File already exists: {preview.path}
            </p>
          )}
          {blockedReason !== null && (
            <p role="alert" className="m-0 mt-8 text-12 text-warning">
              {blockedReason}
            </p>
          )}
        </div>
        {error !== null && (
          <p role="alert" className="m-0 text-12 text-danger">
            {error}
          </p>
        )}
      </div>
    </Modal>
  );
}
