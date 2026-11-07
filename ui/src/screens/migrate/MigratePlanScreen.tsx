import { useState } from "react";
import type { JSX } from "react";
import { Badge, Button, Checkbox, RadioCardGroup } from "../../components";
import type { MigratePlanView } from "../../generated/MigratePlanView";
import { Modal } from "../../overlays/dialogs/Modal";
import { describeCause } from "./applyJob";

const ACTION_LABEL: Readonly<Record<string, string>> = {
  backuporiginal: "Back up original",
  createstoreitem: "Copy into store",
  updatemanifest: "Update store manifest",
  writelink: "Replace original with a link",
  writecopy: "Write a managed copy",
  recordwatched: "Leave original and watch it",
  rendermcp: "Rewrite MCP config",
  renderrule: "Rewrite rule document",
};

/** Human label and badge tone for one receiving scope. */
const SCOPE_LABEL: Readonly<Record<string, { readonly label: string; readonly tone: "info" | "warning" | "locked" | "neutral" }>> = {
  global: { label: "Global", tone: "locked" },
  user: { label: "User", tone: "info" },
  project: { label: "Project", tone: "warning" },
  local: { label: "Local", tone: "neutral" },
};

const STRATEGY_OPTIONS = [
  {
    value: "replace_with_links",
    title: "Replace with links",
    description:
      "Skills become symlinks into the store. MCP entries are rewritten by AgenTuna and marked managed.",
    note: "Recommended",
  },
  {
    value: "leave_and_watch",
    title: "Leave originals",
    description:
      "AgenTuna copies items into the store and watches the originals for drift. Nothing in agent folders changes.",
  },
] as const;

/** Screen three of migration: reviewed strategy, counts and the exact dry run (TC-023). */
export function MigratePlanScreen({
  plan,
  strategy,
  busy,
  error,
  warnings = [],
  onStrategyChange,
  onApply,
  onBack,
  onCancel,
}: {
  readonly plan: MigratePlanView | null;
  readonly strategy: "replace_with_links" | "leave_and_watch";
  readonly busy: boolean;
  readonly error: string | null;
  readonly warnings?: readonly string[];
  readonly onStrategyChange: (strategy: "replace_with_links" | "leave_and_watch") => void;
  readonly onApply: () => Promise<void>;
  readonly onBack: () => void;
  readonly onCancel: () => void;
}): JSX.Element {
  const [showFiles, setShowFiles] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<string | null>(null);

  // The backend resolves every store path from the injected sandbox/store overrides, so the
  // screen never prints a guessed location: a scope is not a directory.
  const backupLocation = plan?.backup_dir ?? "Resolving the backup directory…";

  async function handleConfirmApply() {
    setApplying(true);
    setApplyError(null);
    try {
      await onApply();
      setConfirmOpen(false);
    } catch (err: unknown) {
      setApplyError(describeCause(err));
    } finally {
      setApplying(false);
    }
  }

  return (
    <section aria-label="Migration plan" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-4">
        <p className="m-0 text-12 uppercase tracking-wide text-text-muted">Migrate to AgenTuna</p>
        <h1 className="m-0 text-26 font-semibold text-text">
          {showFiles ? "Preview the exact changes" : "Plan the move"}
        </h1>
        <p className="m-0 text-13 text-text-secondary">
          {showFiles
            ? "Nothing has been written yet. This is the file list Apply will run."
            : "Choose what happens to the originals. Every write is previewed, backed up and reversible."}
        </p>
      </div>

      {error !== null && (
        <div role="alert" className="mt-16 rounded-8 border border-danger/40 bg-danger/10 p-12">
          <p className="m-0 text-13 text-danger">{error}</p>
        </div>
      )}

      <div className="mt-16 grid min-h-0 flex-1 grid-cols-[1fr_360px] gap-16">
        <div className="flex flex-col gap-16">
          <section aria-label="AgenTuna stores" className="rounded-10 border border-border bg-raised p-16">
            <h2 className="m-0 text-14 font-semibold text-text">Where the items land</h2>
            {plan === null ? (
              <p role="status" className="m-0 mt-8 text-13 text-text-secondary">
                Resolving the stores…
              </p>
            ) : (
              <ul className="m-0 mt-8 flex list-none flex-col gap-10 p-0">
                {plan.scopes.map((entry) => {
                  const meta = SCOPE_LABEL[entry.scope] ?? { label: entry.scope, tone: "neutral" as const };
                  return (
                    <li
                      key={entry.scope}
                      aria-label={`${meta.label} store`}
                      className="flex flex-col gap-4 rounded-6 border border-border p-8"
                    >
                      <div className="flex items-center gap-12">
                        <Badge tone={meta.tone} dot>
                          {meta.label}
                        </Badge>
                        <span
                          className="truncate font-mono text-13 text-text-secondary"
                          title={entry.store_path}
                        >
                          {entry.store_path}
                        </span>
                      </div>
                      <p className="m-0 text-12 text-text-muted">
                        {entry.counts.items} item{entry.counts.items === 1 ? "" : "s"} ·{" "}
                        {entry.counts.links_written} link{entry.counts.links_written === 1 ? "" : "s"} ·{" "}
                        {entry.counts.files_backed_up} backup{entry.counts.files_backed_up === 1 ? "" : "s"}
                      </p>
                    </li>
                  );
                })}
              </ul>
            )}
            <p className="m-0 mt-8 text-12 text-text-muted">
              Each item moves into the store of the scope it was found in; unselected files keep their
              path.
            </p>
          </section>

          <section aria-label="Original files" className="rounded-10 border border-border bg-raised p-16">
            <h2 className="m-0 text-14 font-semibold text-text">Original files</h2>
            <div className="mt-8">
              <RadioCardGroup
                label="Original file handling"
                value={strategy}
                columns={2}
                onValueChange={(value) =>
                  onStrategyChange(value === "leave_and_watch" ? "leave_and_watch" : "replace_with_links")
                }
                options={STRATEGY_OPTIONS.map((option) => ({
                  value: option.value,
                  title: option.title,
                  description: option.description,
                  badge: "note" in option ? <Badge tone="success">{option.note}</Badge> : undefined,
                }))}
              />
            </div>
          </section>
        </div>

        <section aria-label="What will happen" className="flex flex-col gap-8 rounded-10 border border-border bg-raised p-16">
          <h2 className="m-0 text-14 font-semibold text-text">What will happen</h2>
          {plan === null ? (
            <p role="status" className="m-0 text-13 text-text-secondary">
              {busy ? "Building the reviewed plan…" : "No plan yet."}
            </p>
          ) : (
            <>
              <ul className="m-0 flex list-none flex-col gap-6 p-0 text-13 text-text">
                <li>{plan.counts.items} item{plan.counts.items === 1 ? "" : "s"} copied into the store</li>
                <li>{plan.counts.links_written} skill folders replaced by links</li>
                <li>{plan.counts.files_created} files written in total</li>
                <li>{plan.counts.files_backed_up} backups kept</li>
                <li>{plan.counts.files_left_in_place} originals left in place</li>
              </ul>
              <p className="m-0 mt-8 text-12 text-text-muted">
                Dry run: {plan.files_checked} file{plan.files_checked === 1 ? "" : "s"} checked, nothing written yet.
              </p>
              <Button size="sm" variant="secondary" onClick={() => setShowFiles((current) => !current)}>
                {showFiles ? "Hide file list" : "Show file list"}
              </Button>
              {showFiles && (
                <div
                  className="m-0 flex max-h-[320px] flex-col gap-10 overflow-auto"
                  aria-label="Planned files"
                >
                  {plan.scopes.map((entry) => {
                    const meta = SCOPE_LABEL[entry.scope] ?? { label: entry.scope, tone: "neutral" as const };
                    return (
                      <div key={entry.scope} className="flex flex-col gap-4" aria-label={`${meta.label} store files`}>
                        <p className="m-0 text-11 uppercase tracking-wide text-text-muted">
                          {meta.label} store · {entry.store_path}
                        </p>
                        <ol className="m-0 flex list-none flex-col gap-4 p-0">
                          {entry.steps.map((step) => (
                            <li key={`${entry.scope}:${step.seq}`} className="rounded-6 border border-border p-8">
                              <div className="flex items-center justify-between gap-6">
                                <span className="text-12 text-text">
                                  {step.seq}. {ACTION_LABEL[step.action] ?? step.action}
                                </span>
                                <span className="font-mono text-11 text-text-muted">{step.bytes} B</span>
                              </div>
                              {step.source !== null && (
                                <p className="m-0 truncate font-mono text-11 text-text-muted" title={step.source}>
                                  from {step.source}
                                </p>
                              )}
                              {step.destination !== null && (
                                <p className="m-0 truncate font-mono text-11 text-text-muted" title={step.destination}>
                                  to {step.destination}
                                </p>
                              )}
                              {step.backup_path !== null && (
                                <p className="m-0 truncate font-mono text-11 text-text-muted" title={step.backup_path}>
                                  backup {step.backup_path}
                                </p>
                              )}
                            </li>
                          ))}
                        </ol>
                      </div>
                    );
                  })}
                </div>
              )}
              <p className="m-0 mt-8 text-11 text-text-muted">
                Plan hash {plan.plan_hash.slice(0, 8)} across {plan.scopes.length} store
                {plan.scopes.length === 1 ? "" : "s"}
              </p>
            </>
          )}
        </section>
      </div>

      <footer className="mt-16 flex items-center justify-between gap-8 border-t border-border pt-16">
        <Button variant="secondary" onClick={onBack}>
          Back
        </Button>
        <div className="flex items-center gap-8">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          {!showFiles ? (
            <Button variant="secondary" disabled={plan === null || busy} onClick={() => setShowFiles(true)}>
              Preview
            </Button>
          ) : null}
          <Button
            variant="primary"
            disabled={plan === null || busy}
            onClick={() => {
              setConfirmed(false);
              setApplyError(null);
              setConfirmOpen(true);
            }}
          >
            {busy ? "Planning…" : `Apply ${plan?.counts.items ?? 0} items`}
          </Button>
        </div>
      </footer>

      {confirmOpen && plan !== null && (
        <Modal
          open={true}
          onOpenChange={(open) => {
            if (!open) setConfirmOpen(false);
          }}
          title="Confirm migration"
          description="Review before writing changes to disk."
          footer={
            <div className="flex items-center justify-end gap-8">
              <Button variant="ghost" disabled={applying} onClick={() => setConfirmOpen(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                disabled={!confirmed || applying}
                onClick={() => void handleConfirmApply()}
              >
                {applying ? "Applying…" : "Apply migration"}
              </Button>
            </div>
          }
        >
          <div className="flex flex-col gap-16 py-8">
            <div className="rounded-8 border border-border bg-raised p-12">
              <h3 className="m-0 text-13 font-semibold text-text">Migration summary</h3>
              <ul className="m-0 mt-8 flex list-disc flex-col gap-4 pl-16 text-12 text-text">
                {plan.scopes.map((entry) => (
                  <li key={entry.scope}>
                    {entry.counts.items} item{entry.counts.items === 1 ? "" : "s"} into the{" "}
                    {(SCOPE_LABEL[entry.scope] ?? { label: entry.scope }).label} store ({entry.store_path})
                  </li>
                ))}
                <li>{plan.counts.links_written} skill folder links to be written</li>
                <li>{plan.counts.files_backed_up} files backed up before modification</li>
              </ul>
            </div>

            <div className="rounded-8 border border-border bg-raised p-12">
              <span className="text-11 font-semibold uppercase tracking-wide text-text-muted">
                Backup location
              </span>
              <p className="m-0 mt-4 font-mono text-12 text-text">{backupLocation}</p>
              <p className="m-0 mt-2 text-11 text-text-muted">
                Original files will be backed up here before anything is changed.
              </p>
            </div>

            {warnings.length > 0 && (
              <div role="alert" className="flex flex-col gap-4 rounded-8 border border-warning/40 bg-warning/10 p-12 text-12 text-warning">
                <span className="font-semibold">Notice:</span>
                {warnings.map((w, idx) => (
                  <p key={idx} className="m-0">{w}</p>
                ))}
              </div>
            )}

            <div className="mt-4">
              <Checkbox
                checked={confirmed}
                onCheckedChange={(checked) => setConfirmed(checked === true)}
                label={`I understand that originals will be backed up to ${backupLocation} and replaced according to this plan.`}
              />
            </div>

            {applyError && (
              <p role="alert" className="m-0 text-12 text-danger">
                {applyError}
              </p>
            )}
          </div>
        </Modal>
      )}
    </section>
  );
}
