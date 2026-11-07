import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, MarkdownEditor } from "../../components";
import type { MarkdownEditorHandle, MarkdownEditorMode } from "../../components";
import { DiffViewer } from "../../components/diff/diff-viewer";
import type { InstructionConflictView } from "../../generated/InstructionConflictView";
import type { InstructionFileView } from "../../generated/InstructionFileView";
import type { InstructionSaveOutcome } from "../../generated/InstructionSaveOutcome";
import type { ExternalRefreshEvent } from "../../generated/ExternalRefreshEvent";
import type { InstructionSaveRequest } from "../../generated/InstructionSaveRequest";
import { Modal } from "../../overlays";
import { NewRuleDialog } from "../../overlays/dialogs/NewRuleDialog";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";

const SCOPE_LABEL: Readonly<Record<string, string>> = {
  global: "Global",
  user: "User",
  project: "Project",
  local: "Local",
};
const SCOPE_BADGE: Readonly<Record<string, "locked" | "info" | "warning" | "success">> = {
  global: "locked",
  user: "info",
  project: "warning",
  local: "success",
};
const SCOPE_DOT: Readonly<Record<string, string>> = {
  global: "bg-scope-global",
  user: "bg-scope-user",
  project: "bg-scope-project",
  local: "bg-scope-local",
};

/** Short, design-shaped description of what one rule file is for. */
function rulePurpose(file: InstructionFileView): string {
  const consumers = file.agents.length > 0 ? ` (${file.agents.join(", ")})` : "";
  const name = file.path.split(/[\\/]/).at(-1) ?? file.path;
  if (file.scope === "global") return `Team coding standards${consumers}`;
  if (file.scope === "user") return `Personal preferences${consumers}`;
  if (file.scope === "local") return `Machine-only overrides${consumers}`;
  if (name.toUpperCase() === "AGENTS.MD") return `Repo conventions${consumers}`;
  return `Repo conventions${consumers}`;
}

function fileName(path: string): string {
  return path.split(/[\\/]/).at(-1) ?? path;
}

/** Scope-grouped rule browser and safe markdown editor. */
export function InstructionsScreen() {
  const { activeProject, route, screenState, externalChanges, scope } = useAppSnapshot();
  const app = useAppState();
  const toast = useToast();
  const [files, setFiles] = useState<readonly InstructionFileView[]>([]);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const requestedItemId = typeof screenState["detail:itemId"] === "string"
    ? screenState["detail:itemId"]
    : null;
  const [buffer, setBuffer] = useState("");
  const [mode, setMode] = useState<MarkdownEditorMode>("edit");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState<InstructionConflictView | null>(null);
  const [externalReloadOpen, setExternalReloadOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(() => screenState["instructions:newRuleOpen"] === true);
  useEffect(() => {
    const openNewRule = () => setCreateOpen(true);
    window.addEventListener("agentuna:open-new-rule", openNewRule);
    return () => window.removeEventListener("agentuna:open-new-rule", openNewRule);
  }, []);
  useEffect(() => {
    if (screenState["instructions:newRuleOpen"] === true) {
      setCreateOpen(true);
      app.setScreenState("instructions:newRuleOpen", false);
    }
  }, [app, screenState]);
  const editor = useRef<MarkdownEditorHandle | null>(null);
  const selected = useMemo(
    () => files.find((file) => file.path === selectedPath) ?? null,
    [files, selectedPath],
  );
  const savedValue = selected?.content ?? "";
  const dirty = selected !== null && selected.read_error === null && buffer !== savedValue;
  const externalChange = externalChanges[route] ?? null;
  const lineDelta = useMemo(() => {
    if (!dirty) return 0;
    const before = savedValue.split("\n").length;
    const after = buffer.split("\n").length;
    return after - before;
  }, [buffer, dirty, savedValue]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<InstructionFileView[]>("instructions_list", {
        projectId: activeProject,
      });
      setFiles(result);
      setSelectedPath((old) => {
        const requested = result.find((file) => file.item_id === requestedItemId);
        return requested?.path ?? (result.some((file) => file.path === old) ? old : (result[0]?.path ?? null));
      });
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, [activeProject, requestedItemId]);

  const reloadExternal = useCallback(async () => {
    if (selected === null) return;
    setBusy(true);
    try {
      const result = await invoke<InstructionFileView[]>("instructions_list", {
        projectId: activeProject,
      });
      setFiles(result);
      const diskFile = result.find((file) => file.path === selected.path);
      if (diskFile === undefined) {
        setSelectedPath(null);
        setBuffer("");
        app.resolveExternalChange(route, "reload", "");
        setError(`The selected instruction file was removed outside AgenTuna: ${selected.path}`);
      } else {
        setSelectedPath(diskFile.path);
        setBuffer(diskFile.content);
        app.resolveExternalChange(route, "reload", diskFile.content);
        setError(null);
      }
      setExternalReloadOpen(false);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }, [activeProject, app, route, selected]);

  useEffect(() => {
    void load();
  }, [load]);
  useEffect(() => {
    setBuffer(selected?.content ?? "");
    setConflict(null);
  }, [selected?.path, selected?.content]);

  useEffect(() => {
    const handleExternalRefresh = (event: Event) => {
      const payload = (event as CustomEvent<ExternalRefreshEvent>).detail;
      if (selected === null) {
        void load();
        return;
      }
      const affectsSelected = payload.full_rescan || payload.paths.includes(selected.path);
      if (dirty && affectsSelected) {
        app.markExternalChange(route, selected.hash);
      } else if (affectsSelected) {
        void reloadExternal();
      } else {
        void load();
      }
    };
    window.addEventListener("agentuna:external-refresh", handleExternalRefresh);
    return () => window.removeEventListener("agentuna:external-refresh", handleExternalRefresh);
  }, [app, dirty, load, reloadExternal, route, selected]);

  const save = useCallback(
    async (options: { readonly force?: boolean } = {}): Promise<InstructionSaveOutcome> => {
      if (selected === null || selected.read_error !== null) {
        throw new Error("This file cannot be saved.");
      }
      const request: InstructionSaveRequest = {
        item_id: selected.item_id,
        path: selected.path,
        scope: selected.scope,
        content: buffer,
        expected_hash: selected.hash,
        project_id: activeProject,
        force: options.force === true,
      };
      const outcome = await invoke<InstructionSaveOutcome>("instruction_save", { request });
      if (outcome.status === "conflict" && outcome.conflict !== null) {
        setConflict(outcome.conflict);
      }
      if (outcome.status === "unreadable") {
        setError(
          `Permission denied: ${outcome.unreadable_path ?? selected.path} cannot be written.`,
        );
      }
      return outcome;
    },
    [activeProject, buffer, selected],
  );

  async function saveInteractive(): Promise<void> {
    if (selected === null || !dirty) return;
    setBusy(true);
    setError(null);
    try {
      const outcome = await save();
      if (outcome.status === "conflict") {
        toast.warning("The file changed on disk. Review the diff before overwriting.", {
          title: "External edit detected",
        });
      } else if (outcome.status === "saved" || outcome.status === "unchanged") {
        toast.success(
          outcome.backup_path === null || outcome.backup_path === undefined
            ? `${fileName(selected.path)} saved`
            : `${fileName(selected.path)} saved · backup ${outcome.backup_path}`,
        );
        setConflict(null);
        await load();
      }
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Instruction file not saved" });
    } finally {
      setBusy(false);
    }
  }

  async function overwrite(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const outcome = await save({ force: true });
      if (outcome.status === "saved" || outcome.status === "unchanged") {
        toast.success(
          outcome.backup_path === null || outcome.backup_path === undefined
            ? `${fileName(selected?.path ?? "")} overwritten`
            : `${fileName(selected?.path ?? "")} overwritten · backup ${outcome.backup_path}`,
        );
        setConflict(null);
        await load();
      }
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Instruction file not saved" });
    } finally {
      setBusy(false);
    }
  }

  function reloadFromDisk(): void {
    if (conflict === null) return;
    setBuffer(conflict.disk_content);
    setFiles((current) =>
      current.map((file) =>
        file.path === conflict.path
          ? { ...file, content: conflict.disk_content, hash: conflict.disk_hash }
          : file,
      ),
    );
    setConflict(null);
    toast.info("Reloaded the version on disk. Your buffer was replaced.");
  }


  // The screen registers its buffer so leaving with unsaved changes asks for a decision.
  useEffect(() => {
    if (selected === null || selected.read_error !== null) return;
    app.setEditorBuffer(route, {
      route,
      value: buffer,
      savedValue,
      onSave: async () => {
        const outcome = await save();
        if (outcome.status !== "saved" && outcome.status !== "unchanged") {
          throw new Error("Save did not complete.");
        }
        await load();
      },
    });
  }, [app, buffer, load, route, save, savedValue, selected]);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void saveInteractive();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });

  const scopes = ["global", "user", "project", "local"] as const;
  const grouped = scopes
    .map((scope) => ({ scope, items: files.filter((file) => file.scope === scope) }))
    .filter((group) => group.items.length > 0);

  return (
    <main aria-label="Instructions" className="mx-auto flex max-w-[1180px] flex-col gap-20 p-24">
      <header className="flex items-start justify-between gap-16">
        <div>
          <h1 className="m-0 text-22 font-semibold text-text">Instructions</h1>
          <p className="m-0 mt-4 text-13 text-text-secondary">
            Rule files your agents read at startup. Agents read every file below in this order.
          </p>
        </div>
        <Button variant="primary" disabled={busy} onClick={() => setCreateOpen(true)}>
          <span aria-hidden="true">＋</span> New rule file
        </Button>
      </header>
      {externalChange !== null && selected !== null && (
        <section role="alert" className="flex flex-wrap items-center justify-between gap-8 rounded-8 border border-warning/30 bg-warning/10 p-12 text-13">
          <span>
            <strong>Changed outside AgenTuna.</strong>{" "}
            {externalChange.kept
              ? "Your edits are kept; the external version remains on disk and is marked as drift."
              : `The open file changed while you were editing: ${selected.path}`}
          </span>
          <span className="flex gap-8">
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => setExternalReloadOpen(true)}>Reload</Button>
            {!externalChange.kept && <Button size="sm" disabled={busy} onClick={() => app.resolveExternalChange(route, "keep")}>Keep mine</Button>}
          </span>
        </section>
      )}

      {loading ? (
        <p role="status" className="text-13 text-text-secondary">
          Loading instruction files…
        </p>
      ) : error !== null && files.length === 0 ? (
        <section role="alert" className="rounded-10 border border-border bg-raised p-24">
          <p className="m-0 text-13 text-danger">{error}</p>
          <Button className="mt-12" onClick={() => void load()}>
            Retry
          </Button>
        </section>
      ) : files.length === 0 ? (
        <section className="rounded-10 border border-border bg-raised p-24">
          <h2 className="m-0 text-16 font-semibold">No rule files found</h2>
          <p className="mt-8 text-13 text-text-secondary">
            Create a rule file through the New rule action.
          </p>
          <Button variant="primary" disabled={busy} onClick={() => setCreateOpen(true)}>
            New rule file
          </Button>
        </section>
      ) : (
        <div className="grid min-h-[620px] grid-cols-[320px_1fr] gap-16">
          <aside
            aria-label="Instruction files"
            className="flex flex-col gap-8 rounded-10 border border-border bg-raised p-8"
          >
            {grouped.map(({ scope, items }) => (
              <section key={scope} aria-label={`${SCOPE_LABEL[scope] ?? scope} rule files`}>
                <ul className="m-0 flex list-none flex-col gap-4 p-0">
                  {items.map((file) => {
                    const active = file.path === selectedPath;
                    return (
                      <li key={file.path}>
                        <button
                          type="button"
                          aria-pressed={active}
                          aria-label={fileName(file.path)}
                          className={`flex w-full flex-col gap-6 rounded-8 border p-12 text-left transition-colors focus-visible:outline-2 focus-visible:outline-focus ${
                            active
                              ? "border-accent bg-overlay"
                              : "border-transparent hover:border-border hover:bg-overlay/60"
                          }`}
                          onClick={() => {
                            app.recordVisitedItem(`rule:${file.item_id}`);
                            if (dirty) app.setEditorBuffer(route, {
                              route,
                              value: buffer,
                              savedValue,
                              onSave: async () => {
                                await saveInteractive();
                              },
                            });
                            setSelectedPath(file.path);
                          }}
                        >
                          <span className="flex items-center justify-between gap-8">
                            <span className="inline-flex items-center gap-6 text-12 text-text-secondary">
                              <span
                                aria-hidden="true"
                                className={`size-8 rounded-full ${SCOPE_DOT[file.scope] ?? "bg-text-muted"}`}
                              />
                              {SCOPE_LABEL[file.scope] ?? file.scope}
                            </span>
                            <span className="min-w-0 truncate text-11 text-text-muted">
                              {rulePurpose(file)}
                            </span>
                          </span>
                          <span className="flex items-center gap-8">
                            <span className="min-w-0 truncate font-mono text-13 text-text">
                              {fileName(file.path)}
                            </span>
                            {file.read_error !== null && (
                              <Badge tone="danger" dot>
                                Unreadable
                              </Badge>
                            )}
                          </span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </section>
            ))}
          </aside>

          <section className="flex min-w-0 flex-col rounded-10 border border-border bg-raised">
            {selected === null ? (
              <div className="p-16">
                {error !== null && <p role="alert" className="m-0 mb-8 text-13 text-danger">{error}</p>}
                <p className="m-0 text-13 text-text-secondary">Select a rule file.</p>
              </div>
            ) : (
              <>
                <div className="flex flex-wrap items-center justify-between gap-8 border-b border-border p-12">
                  <div className="flex min-w-0 items-center gap-8">
                    <span className="truncate font-mono text-15 font-semibold text-text">
                      ./{fileName(selected.path)}
                    </span>
                    <Badge tone={SCOPE_BADGE[selected.scope] ?? "neutral"} dot>
                      {SCOPE_LABEL[selected.scope] ?? selected.scope}
                    </Badge>
                  </div>
                </div>
                <div className="min-h-0 flex-1 p-12">
                  {selected.read_error !== null ? (
                    <div role="alert" className="rounded-8 border border-danger/40 bg-danger/10 p-12">
                      <p className="m-0 text-13 text-danger">
                        {selected.read_error}: <span className="font-mono">{selected.path}</span>
                      </p>
                      <Button size="sm" className="mt-8" onClick={() => void load()}>
                        Rescan
                      </Button>
                    </div>
                  ) : (
                    <MarkdownEditor
                      ref={editor}
                      label="Instruction content"
                      value={buffer}
                      baseline={savedValue}
                      mode={mode}
                      onModeChange={setMode}
                      onChange={setBuffer}
                      onSave={() => void saveInteractive()}
                      className="min-h-[420px]"
                    />
                  )}
                  {error !== null && files.length > 0 && (
                    <p role="alert" className="m-0 mt-8 text-13 text-danger">
                      {error}
                    </p>
                  )}
                </div>
                <footer className="flex items-center justify-between gap-8 border-t border-border p-12">
                  <span
                    role="status"
                    aria-label={dirty ? "Unsaved changes" : "Saved"}
                    className={`text-12 ${dirty ? "text-warning" : "text-text-muted"}`}
                  >
                    {dirty
                      ? `Unsaved changes · ${Math.abs(lineDelta)} line${Math.abs(lineDelta) === 1 ? "" : "s"} ${lineDelta >= 0 ? "added" : "removed"}`
                      : "No unsaved changes"}
                  </span>
                  <div className="flex gap-8">
                    <Button
                      variant="ghost"
                      disabled={!dirty || busy}
                      onClick={() => setBuffer(savedValue)}
                    >
                      Discard
                    </Button>
                    <Button
                      variant="primary"
                      disabled={!dirty || busy || selected.read_error !== null}
                      onClick={() => void saveInteractive()}
                    >
                      {busy ? "Saving…" : "Save"}
                    </Button>
                  </div>
                </footer>
              </>
            )}
          </section>
        </div>
      )}

      <Modal
        open={conflict !== null}
        title="File changed on disk"
        description="Another program edited this file after you opened it. Reload to keep their version, or overwrite it with your buffer."
        size="lg"
        onOpenChange={(open) => {
          if (!open) setConflict(null);
        }}
        footer={
          <>
            <Button variant="ghost" onClick={() => setConflict(null)}>
              Cancel
            </Button>
            <Button variant="secondary" onClick={reloadFromDisk}>
              Reload
            </Button>
            <Button variant="destructive" disabled={busy} onClick={() => void overwrite()}>
              Overwrite
            </Button>
          </>
        }
      >
        {conflict !== null && (
          <>
            <p className="m-0 mb-8 font-mono text-12 text-text-muted">{conflict.path}</p>
            <DiffViewer diff={conflict.diff} displayPath={fileName(conflict.path)} />
            <p className="m-0 mt-8 text-12 text-text-secondary">
              Overwrite saves your buffer and keeps a backup of the current file.
            </p>
          </>
        )}
      </Modal>
      <Modal
        open={externalReloadOpen}
        title="Reload external changes?"
        description="This replaces your unsaved edits with the version currently on disk."
        size="md"
        onOpenChange={(open) => setExternalReloadOpen(open)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setExternalReloadOpen(false)}>Cancel</Button>
            <Button variant="primary" disabled={busy} onClick={() => void reloadExternal()}>Reload file</Button>
          </>
        }
      >
        <p className="m-0 text-13 text-text-secondary">{selected?.path}</p>
      </Modal>
      <NewRuleDialog
        open={createOpen}
        project={activeProject}
        initialScope={scope}
        onClose={() => { setCreateOpen(false); app.setScreenState("instructions:newRuleOpen", false); }}
        onCreated={async (created) => {
          await load();
          setSelectedPath(created);
        }}
      />
    </main>
  );
}
