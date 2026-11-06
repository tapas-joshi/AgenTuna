import { useState } from "react";
import { Badge, Button, RadioCardGroup } from "../../components";
import type { MigratePlanView } from "../../generated/MigratePlanView";

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

/** Screen three of migration: reviewed strategy, counts and the exact dry run. */
export function MigratePlanScreen({
  plan,
  strategy,
  busy,
  error,
  onStrategyChange,
  onBack,
  onCancel,
}: {
  readonly plan: MigratePlanView | null;
  readonly strategy: "replace_with_links" | "leave_and_watch";
  readonly busy: boolean;
  readonly error: string | null;
  readonly onStrategyChange: (strategy: "replace_with_links" | "leave_and_watch") => void;
  readonly onBack: () => void;
  readonly onCancel: () => void;
}) {
  const [showFiles, setShowFiles] = useState(false);
  return (
    <main aria-label="Migration plan" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-4">
        <p className="m-0 text-12 uppercase tracking-wide text-text-muted">Migrate to AgenTuna</p>
        <h1 className="m-0 text-26 font-semibold text-text">Plan the move</h1>
        <p className="m-0 text-13 text-text-secondary">
          Choose what happens to the originals. Every write is previewed, backed up and reversible.
        </p>
      </div>

      {error !== null && (
        <div role="alert" className="mt-16 rounded-8 border border-danger/40 bg-danger/10 p-12">
          <p className="m-0 text-13 text-danger">{error}</p>
        </div>
      )}

      <div className="mt-16 grid min-h-0 flex-1 grid-cols-[1fr_360px] gap-16">
        <div className="flex flex-col gap-16">
          <section aria-label="AgenTuna store" className="rounded-10 border border-border bg-raised p-16">
            <h2 className="m-0 text-14 font-semibold text-text">AgenTuna store</h2>
            <div className="mt-8 flex items-center gap-12">
              <Badge tone={plan?.scope === "project" ? "warning" : "info"} dot>
                {plan?.scope === "project" ? "Project" : "User"}
              </Badge>
              <span className="font-mono text-13 text-text-secondary">
                {plan?.scope === "project" ? "./.config-studio/" : "~/.config-studio/"}
              </span>
            </div>
            <p className="m-0 mt-8 text-12 text-text-muted">
              Selected originals change only as reviewed here; unselected files keep their path.
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
                <ol className="m-0 flex max-h-[320px] list-none flex-col gap-4 overflow-auto p-0">
                  {plan.steps.map((step) => (
                    <li key={step.seq} className="rounded-6 border border-border p-8">
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
              )}
              <p className="m-0 mt-8 text-11 text-text-muted">
                Plan {plan.plan_id.slice(-8)} · hash {plan.plan_hash.slice(0, 8)}
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
          <Button variant="primary" disabled={plan === null || busy} onClick={() => setShowFiles(true)}>
            {busy ? "Planning…" : "Preview"}
          </Button>
        </div>
      </footer>
    </main>
  );
}
