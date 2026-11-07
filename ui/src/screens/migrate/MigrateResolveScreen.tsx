import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, Input } from "../../components";
import { DiffViewer } from "../../components/diff/diff-viewer";
import type { MigrateConflictCopyView } from "../../generated/MigrateConflictCopyView";
import type { MigrateConflictView } from "../../generated/MigrateConflictView";
import type { MigrateConflictsView } from "../../generated/MigrateConflictsView";
import type { MigrateResolutionView } from "../../generated/MigrateResolutionView";
import { describeCause } from "./applyJob";

/** One reviewed conflict decision. */
export type ConflictChoice = "keep_a" | "keep_b" | "keep_both";

/** Choices keyed by `kind:name`. */
export type ConflictChoices = Readonly<Record<string, ConflictChoice>>;

/** Keep-both names keyed by `kind:name`. */
export type ConflictNames = Readonly<Record<string, readonly [string, string]>>;

const NAME_PATTERN = /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$/;

/** Stable key for one conflict group. */
export function conflictKey(conflict: MigrateConflictView): string {
  return `${conflict.kind}:${conflict.name}`;
}

/** Converts reviewed choices into the backend resolution payload. */
export function resolutionPayload(
  conflicts: readonly MigrateConflictView[],
  choices: ConflictChoices,
  names: ConflictNames,
): MigrateResolutionView[] {
  return conflicts.map((conflict) => {
    const key = conflictKey(conflict);
    const choice = choices[key] ?? (conflict.formatting_only ? "keep_a" : "keep_both");
    return {
      name: conflict.name,
      choice,
      names: choice === "keep_both" ? [...(names[key] ?? defaultNames(conflict))] : [],
    };
  });
}

/** Two starting names for Keep both, derived from the copies' agents. */
export function defaultNames(conflict: MigrateConflictView): [string, string] {
  const base = conflict.name.replace(/[^a-zA-Z0-9._-]/g, "-");
  const a = conflict.copies[0]?.agent.replaceAll("_", "-") ?? "a";
  const b = conflict.copies[1]?.agent.replaceAll("_", "-") ?? "b";
  return [`${base}-${a}`, `${base}-${b}`];
}

function shortHash(hash: string): string {
  return hash.slice(0, 8);
}

function CopyColumn({ label, copy }: { readonly label: string; readonly copy: MigrateConflictCopyView }) {
  return (
    <div className="min-w-0 rounded-8 border border-border p-10">
      <div className="flex items-center justify-between gap-6">
        <strong className="text-13 text-text">Copy {label}</strong>
        <Badge tone={copy.scope === "project" ? "warning" : copy.scope === "global" ? "locked" : "info"} dot>
          {copy.scope}
        </Badge>
      </div>
      <p className="m-0 mt-4 text-12 text-text-secondary">{copy.agent}</p>
      <p className="m-0 truncate font-mono text-11 text-text-muted" title={copy.path}>
        {copy.path}
      </p>
      <p className="m-0 mt-2 font-mono text-11 text-text-muted">sha {shortHash(copy.raw_hash)}</p>
    </div>
  );
}

/** Screen two of migration: compare conflicting native copies and record explicit choices. */
export function MigrateResolveScreen({
  projectId,
  selectedPaths,
  choices,
  names,
  onChoicesChange,
  onNamesChange,
  onBack,
  onContinue,
}: {
  readonly projectId: string | null;
  readonly selectedPaths: readonly string[];
  readonly choices: ConflictChoices;
  readonly names: ConflictNames;
  readonly onChoicesChange: (choices: ConflictChoices) => void;
  readonly onNamesChange: (names: ConflictNames) => void;
  readonly onBack: () => void;
  readonly onContinue: (conflicts: readonly MigrateConflictView[]) => void;
}) {
  const [conflicts, setConflicts] = useState<readonly MigrateConflictView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const selectedKey = useMemo(() => selectedPaths.join("\n"), [selectedPaths]);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    invoke<MigrateConflictsView>("migrate_conflicts", {
      projectId,
      selectedPaths: [...selectedPaths],
    }).then(
      (result) => {
        if (!active) return;
        setConflicts(result.conflicts);
        const seeded: Record<string, ConflictChoice> = { ...choices };
        const seededNames: Record<string, readonly [string, string]> = { ...names };
        for (const conflict of result.conflicts) {
          const key = conflictKey(conflict);
          if (conflict.formatting_only) seeded[key] = "keep_a";
          if (seeded[key] === "keep_both") seededNames[key] = seededNames[key] ?? defaultNames(conflict);
        }
        onChoicesChange(seeded);
        onNamesChange(seededNames);
      },
      (cause: unknown) => {
        if (active) setError(describeCause(cause));
      },
    ).finally(() => {
      if (active) setLoading(false);
    });
    return () => {
      active = false;
    };
    // Choices and names are seeded once per scan; they are not dependencies of the fetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, selectedKey]);

  const nameErrors = useMemo(() => {
    const errors: Record<string, string> = {};
    const seen = new Map<string, string>();
    for (const conflict of conflicts) {
      const key = conflictKey(conflict);
      if (choices[key] !== "keep_both") continue;
      const pair = names[key] ?? defaultNames(conflict);
      for (const [index, name] of pair.entries()) {
        if (!NAME_PATTERN.test(name)) {
          errors[key] = `Copy ${index === 0 ? "A" : "B"} name must be 1–64 letters, digits, dots, hyphens or underscores.`;
          break;
        }
        if (pair[0] === pair[1]) {
          errors[key] = "The two names must differ.";
          break;
        }
        const owner = seen.get(name.toLowerCase());
        if (owner !== undefined && owner !== key) {
          errors[key] = `“${name}” is already used by another conflict.`;
          break;
        }
      }
      seen.set(pair[0].toLowerCase(), key);
      seen.set(pair[1].toLowerCase(), key);
    }
    return errors;
  }, [choices, conflicts, names]);

  const unresolved = conflicts.some((conflict) => {
    const key = conflictKey(conflict);
    return choices[key] === undefined || nameErrors[key] !== undefined;
  });

  return (
    <section aria-label="Resolve migration conflicts" className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-4">
        <p className="m-0 text-12 uppercase tracking-wide text-text-muted">Migrate to AgenTuna</p>
        <h1 className="m-0 text-26 font-semibold text-text">Resolve conflicts</h1>
        <p className="m-0 text-13 text-text-secondary">
          Two agents hold different copies of the same item. Choose what AgenTuna keeps before the
          plan is built.
        </p>
      </div>

      {loading ? (
        <p role="status" className="mt-20 text-13 text-text-secondary">
          Comparing selected copies…
        </p>
      ) : error !== null ? (
        <section role="alert" className="mt-20 rounded-10 border border-border bg-raised p-16">
          <p className="m-0 text-13 text-danger">{error}</p>
          <Button className="mt-8" onClick={onBack}>
            Retry comparison
          </Button>
        </section>
      ) : conflicts.length === 0 ? (
        <section className="mt-20 rounded-10 border border-border bg-raised p-24">
          <h2 className="m-0 text-16 font-semibold">No conflicts among selected items</h2>
          <p className="mt-8 text-13 text-text-secondary">
            The selected copies can continue to planning without a conflict choice.
          </p>
        </section>
      ) : (
        <>
          <div className="mt-16 flex flex-wrap items-center gap-8">
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                const next = { ...choices };
                for (const conflict of conflicts) {
                  const key = conflictKey(conflict);
                  if (next[key] === undefined) next[key] = "keep_a";
                }
                onChoicesChange(next);
              }}
            >
              Keep A for unresolved
            </Button>
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                const next = { ...choices };
                const nextNames = { ...names };
                for (const conflict of conflicts) {
                  const key = conflictKey(conflict);
                  if (next[key] === undefined) next[key] = "keep_b";
                }
                onNamesChange(nextNames);
                onChoicesChange(next);
              }}
            >
              Keep B for unresolved
            </Button>
            <span className="ml-auto text-12 text-text-muted">
              {conflicts.filter((conflict) => choices[conflictKey(conflict)] === undefined).length} of{" "}
              {conflicts.length} unresolved
            </span>
          </div>

          <ul className="m-0 mt-12 flex list-none flex-col gap-12 p-0">
            {conflicts.map((conflict) => {
              const key = conflictKey(conflict);
              const choice = choices[key];
              const pair = names[key] ?? defaultNames(conflict);
              const error = nameErrors[key];
              return (
                <li key={key} className="rounded-10 border border-border bg-raised p-12">
                  <header className="flex flex-wrap items-center gap-8">
                    <h2 className="m-0 text-14 font-semibold text-text">
                      {conflict.kind} · {conflict.name}
                    </h2>
                    {conflict.formatting_only && (
                      <Badge tone="success" dot>
                        Equal content after formatting
                      </Badge>
                    )}
                    {choice === undefined && <Badge tone="warning">Needs a choice</Badge>}
                  </header>
                  {conflict.formatting_only && (
                    <p className="m-0 mt-4 text-12 text-text-secondary">
                      Only whitespace and formatting differ, so AgenTuna keeps copy A by default.
                    </p>
                  )}
                  <div className="mt-8 grid grid-cols-2 gap-8">
                    {conflict.copies.slice(0, 2).map((copy, index) => (
                      <CopyColumn key={copy.path} label={index === 0 ? "A" : "B"} copy={copy} />
                    ))}
                  </div>
                  {conflict.unified_diff.length > 0 && (
                    <div className="mt-8">
                      <DiffViewer diff={conflict.unified_diff} displayPath={conflict.name} />
                    </div>
                  )}
                  <fieldset className="m-0 mt-8 border-0 p-0">
                    <legend className="mb-4 text-12 text-text-secondary">Keep</legend>
                    <div className="flex flex-wrap items-center gap-12 text-13 text-text">
                      {(["keep_a", "keep_b", "keep_both"] as const).map((value) => (
                        <label key={value} className="inline-flex items-center gap-6">
                          <input
                            type="radio"
                            name={`resolution-${key}`}
                            checked={choice === value}
                            onChange={() => {
                              onChoicesChange({ ...choices, [key]: value });
                              if (value === "keep_both" && names[key] === undefined) {
                                onNamesChange({ ...names, [key]: defaultNames(conflict) });
                              }
                            }}
                          />
                          {value === "keep_a" ? "Keep A" : value === "keep_b" ? "Keep B" : "Keep both"}
                        </label>
                      ))}
                    </div>
                  </fieldset>
                  {choice === "keep_both" && (
                    <div className="mt-8 flex flex-wrap items-end gap-8">
                      <Input
                        label="Copy A name"
                        value={pair[0]}
                        onChange={(event) =>
                          onNamesChange({ ...names, [key]: [event.target.value, pair[1]] })
                        }
                        className="w-56"
                      />
                      <Input
                        label="Copy B name"
                        value={pair[1]}
                        onChange={(event) =>
                          onNamesChange({ ...names, [key]: [pair[0], event.target.value] })
                        }
                        className="w-56"
                      />
                      {error !== undefined && (
                        <p role="alert" className="m-0 text-12 text-danger">
                          {error}
                        </p>
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        </>
      )}

      <footer className="mt-auto flex items-center justify-between gap-8 border-t border-border pt-16">
        <Button variant="secondary" onClick={onBack}>
          Back to scan
        </Button>
        <div className="flex items-center gap-12">
          {unresolved && conflicts.length > 0 && (
            <span role="status" className="text-12 text-warning">
              Resolve every conflict to continue
            </span>
          )}
          <Button
            variant="primary"
            disabled={loading || error !== null || unresolved}
            onClick={() => onContinue(conflicts)}
          >
            Continue
          </Button>
        </div>
      </footer>
    </section>
  );
}
