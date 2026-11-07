/**
 * Native skill detail (design `02c-skills-shared-root`).
 *
 * A skill that lives only in agent folders has no AgenTuna package to edit, so
 * this page answers the two questions the list cannot: which folders hold it,
 * and which agents read each folder. When a folder is a shared discovery root
 * (Codex and omp over `.agents/skills`) the page states the coupling up front
 * and offers one reviewed adoption step instead of two switches that would lie.
 */
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import { Button } from "../../components/button";
import { Badge } from "../../components/badge";
import { AsyncBoundary, useAsyncResource } from "../../components/states";
import { AdoptModal } from "../../overlays/dialogs/AdoptModal";
import type { AdoptApplyView } from "../../generated/AdoptApplyView";
import type { SkillsListView } from "../../generated/SkillsListView";
import { useAppState, useAppSnapshot } from "../../state/context";

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
  local: "bg-scope-project/15 text-scope-project",
};

const SCOPE_NAME: Readonly<Record<string, string>> = {
  global: "Global",
  user: "User",
  project: "Project",
  local: "Local",
};

/** One agent folder a native skill was found in, with every agent that reads it. */
interface NativeFolder {
  readonly path: string;
  readonly agents: readonly string[];
}

/** Renders `~/.claude/skills/name` as `.claude/skills/name`, keeping the whole path in a tooltip. */
function folderTail(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).slice(-3).join("/");
}

/** Props accepted by {@link NativeSkillDetail}. */
export interface NativeSkillDetailProps {
  /** Folder the user opened from the Skills list. */
  readonly path: string;
  /** Returns to the Skills list. */
  readonly onClose: () => void;
}

/**
 * Detail page for a skill that exists only in agent folders.
 *
 * Every statement comes from the same `skills_list` projection the table renders,
 * so the folders, readers and scope shown here can never disagree with the list.
 */
export function NativeSkillDetail({ path, onClose }: NativeSkillDetailProps) {
  const app = useAppState();
  const { activeProject } = useAppSnapshot();
  const [adoptOpen, setAdoptOpen] = useState(false);
  const resource = useAsyncResource(
    () =>
      invoke<SkillsListView>("skills_list", {
        request: { scope: null, project_id: activeProject },
      }),
    { deps: [path, activeProject] },
  );
  return (
    <AsyncBoundary
      state={resource}
      loadingTitle="Loading folder"
      loadingBody="Reading agent folders and their owners"
      skeletonRows={3}
      errorTitle="Folders could not be read"
      minHeight={360}
      empty={{
        title: "Folder no longer found",
        body: "This skill was adopted, moved or removed from the agent folder. Return to Skills and rescan.",
        action: { label: "Back to Skills", onClick: onClose },
        secondaryAction: { label: "Refresh", onClick: resource.retry },
      }}
    >
      {(view) => {
        const row = view.rows.find(
          (candidate) =>
            candidate.native &&
            (candidate.native_path === path ||
              candidate.agents.some(
                (cell) => cell.state === "native" && cell.config_path === path,
              )),
        );
        if (row === undefined) {
          return (
            <main aria-label="Native skill detail" className="mx-auto flex w-full max-w-[1120px] flex-col gap-16 p-24">
              <Button variant="ghost" onClick={onClose}>
                ← Back to Skills
              </Button>
              <h1 className="m-0 text-22 font-semibold text-text">Folder no longer found</h1>
              <p className="m-0 text-13 text-text-secondary">
                This skill was adopted, moved or removed from the agent folder. Rescan the Skills
                list to see its current state.
              </p>
            </main>
          );
        }
        const labels = new Map(view.agents.map((agent) => [agent.id, agent.label]));
        const folders = new Map<string, string[]>();
        for (const cell of row.agents) {
          if (cell.state !== "native" || cell.config_path === null) continue;
          const readers = folders.get(cell.config_path) ?? [];
          readers.push(labels.get(cell.agent) ?? cell.agent);
          folders.set(cell.config_path, readers);
        }
        const classes: NativeFolder[] = [...folders].map(([folderPath, agents]) => ({
          path: folderPath,
          agents,
        }));
        const shared = classes.filter((folder) => folder.agents.length > 1);
        const scopeLabel = SCOPE_NAME[row.scope] ?? row.scope;
        const readers = [...folders.values()].flat();
        const subtitle =
          classes.length > 1
            ? `${row.name} exists ${classes.length} times: ${classes
                .map((folder) =>
                  folder.agents.length > 1
                    ? `one shared folder that ${folder.agents.join(" and ")} both read`
                    : `an own copy for ${folder.agents.join(" + ")}`,
                )
                .join(", ")}. AgenTuna shows the coupling up front and offers one reviewed adoption step instead of two switches that would lie.`
            : `${row.name} lives only in ${readers.join(" + ")}'s folder. AgenTuna does not toggle, move or delete it; adopt it first.`;
        return (
          <main aria-label="Native skill detail" className="mx-auto flex w-full max-w-[1120px] flex-col gap-16 p-24">
            <div>
              <button
                type="button"
                className="mb-8 border-0 bg-transparent p-0 text-12 text-text-secondary hover:text-text"
                onClick={onClose}
              >
                ← Back to Skills
              </button>
              <div className="flex flex-wrap items-center gap-8">
                <h1 className="m-0 text-26 font-semibold text-text">
                  {shared.length > 0 ? "One folder, two agents" : `${row.name} lives in an agent folder`}
                </h1>
                <span
                  className={`rounded-6 px-8 py-4 text-12 ${SCOPE_BADGE[row.scope] ?? "bg-overlay text-text-secondary"}`}
                >
                  {scopeLabel}
                </span>
                <Badge tone="muted">not in AgenTuna</Badge>
              </div>
              <p className="mb-0 mt-8 text-13 text-text-secondary">{subtitle}</p>
            </div>
            {shared.length > 0 && (
              <section
                role="note"
                aria-label="Shared discovery root"
                className="flex items-start gap-12 rounded-8 border border-scope-user/40 bg-scope-user/10 p-12 text-13 text-text"
              >
                <span aria-hidden="true" className="mt-2 text-scope-user">
                  ⇄
                </span>
                <span>
                  <strong className="block text-13">Shared discovery root</strong>
                  <span className="mt-2 block text-text-secondary">
                    {shared
                      .map((folder) => folderTail(folder.path).split("/").slice(0, -1).join("/"))
                      .join(", ")}{" "}
                    is one folder on disk. Enabling or disabling it changes both agents; AgenTuna
                    shows that coupling and never offers a per-agent switch for it.
                  </span>
                </span>
              </section>
            )}
            <div className="grid gap-16 lg:grid-cols-[minmax(0,1.4fr)_minmax(280px,1fr)]">
              <section
                aria-label="Where it lives"
                className="rounded-10 border border-border bg-raised p-16"
              >
                <h2 className="m-0 text-16 font-semibold text-text">Where it lives</h2>
                <p className="mb-0 mt-4 text-12 text-text-muted">
                  AgenTuna read these paths. Nothing has been written.
                </p>
                <div className="mt-12 grid gap-12">
                  {classes.map((folder) => {
                    const isShared = folder.agents.length > 1;
                    return (
                      <div
                        key={folder.path}
                        className={`rounded-8 border p-12 ${isShared ? "border-scope-user/60 bg-scope-user/5" : "border-border"}`}
                      >
                        <h3 className="m-0 text-14 font-semibold text-text">
                          {folder.agents.join(" + ")}
                        </h3>
                        <p
                          className="mb-0 mt-4 break-all font-mono text-12 text-text-secondary"
                          title={folder.path}
                        >
                          {folderTail(folder.path)}/
                        </p>
                        <p className="mb-0 mt-4 text-12 text-text-muted">
                          {isShared
                            ? "One shared folder · read-only until adopted"
                            : "Own copy · native, not managed"}
                        </p>
                        {isShared && (
                          <span className="mt-8 flex flex-wrap items-center gap-6">
                            {folder.agents.map((agent) => (
                              <Badge key={agent} tone="muted">
                                {agent}
                              </Badge>
                            ))}
                          </span>
                        )}
                      </div>
                    );
                  })}
                </div>
                <p className="mb-0 mt-12 text-12 text-text-muted">
                  Native folder: AgenTuna does not toggle, move or delete it. Adopt first.
                </p>
                <p className="mb-0 mt-4 text-12 text-text-muted">
                  After adoption a shared folder still loads in both agents; one toggle remains.
                </p>
              </section>
              <aside className="flex flex-col gap-16">
                <section className="rounded-10 border border-border bg-raised p-16" aria-label="Ownership">
                  <h2 className="mb-12 mt-0 text-14 font-semibold text-text">Ownership</h2>
                  <dl className="m-0 grid gap-12 text-12">
                    <div>
                      <dt className="text-11 text-text-muted">Not in AgenTuna</dt>
                      <dd className="m-0 mt-2 text-text-secondary">No managed package yet</dd>
                    </div>
                    <div>
                      <dt className="text-11 text-text-muted">Source</dt>
                      <dd className="m-0 mt-2 text-text-secondary">Agent folder (native)</dd>
                    </div>
                    <div>
                      <dt className="text-11 text-text-muted">Read by</dt>
                      <dd className="m-0 mt-2 text-text-secondary">
                        {readers.join(" + ")}
                        {shared.length > 0 ? " · shared root" : ""}
                      </dd>
                    </div>
                    <div>
                      <dt className="text-11 text-text-muted">AgenTuna store</dt>
                      <dd className="m-0 mt-2 text-text-secondary">Not copied yet</dd>
                    </div>
                  </dl>
                </section>
                <section className="rounded-10 border border-border bg-raised p-16" aria-label="What adoption changes">
                  <h2 className="mb-8 mt-0 text-14 font-semibold text-text">What adoption changes</h2>
                  <ol className="m-0 grid list-decimal gap-4 pl-18 text-12 text-text-secondary">
                    <li>Copies the folder into AgenTuna</li>
                    <li>Replaces the original with a link, after a backup</li>
                    <li>One toggle stays; both agents keep loading it</li>
                  </ol>
                  <div className="mt-12 flex flex-wrap gap-8">
                    <Button
                      variant="primary"
                      onClick={() => setAdoptOpen(true)}
                    >
                      Adopt into AgenTuna…
                    </Button>
                    <Button variant="secondary" onClick={onClose}>
                      Leave alone
                    </Button>
                  </div>
                </section>
              </aside>
            </div>
            {adoptOpen && (
              <AdoptModal
                path={path}
                onAdoptSuccess={(result: AdoptApplyView) => {
                  setAdoptOpen(false);
                  app.toasts.show({ kind: "success", title: "Adopted into AgenTuna", message: result.summary });
                  resource.retry();
                }}
                onRescan={resource.retry}
                onClose={() => setAdoptOpen(false)}
              />
            )}
          </main>
        );
      }}
    </AsyncBoundary>
  );
}
