import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, Input, Toggle } from "../../components";
import type { PresetApplicationView } from "../../generated/PresetApplicationView";
import type { PresetApplyResultView } from "../../generated/PresetApplyResultView";
import type { PresetCandidateView } from "../../generated/PresetCandidateView";
import type { PresetView } from "../../generated/PresetView";
import { Modal } from "../../overlays";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot } from "../../state/context";
import { commandErrorMessage } from "../../lib/commandError";

const NAME_PATTERN = /^[a-zA-Z0-9][a-zA-Z0-9 _-]{0,63}$/;

function createdLabel(timestamp: string): string {
  const time = Date.parse(timestamp);
  if (!Number.isFinite(time)) return timestamp;
  return new Date(time).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

/** Preset list, immediate membership editor and reviewed apply/unapply controls. */
export function PresetsScreen() {
  const { activeProject, scope } = useAppSnapshot();
  const toast = useToast();
  const [presets, setPresets] = useState<readonly PresetView[]>([]);
  const [candidates, setCandidates] = useState<readonly PresetCandidateView[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [applied, setApplied] = useState<Readonly<Record<string, PresetApplyResultView>>>({});
  const [createOpen, setCreateOpen] = useState(false);
  const [draftName, setDraftName] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [rows, members] = await Promise.all([
        invoke<PresetView[]>("presets_list"),
        invoke<PresetCandidateView[]>("preset_member_candidates", { scope }),
      ]);
      setPresets(rows);
      setCandidates(members);
      setSelectedId((old) =>
        rows.some((row) => row.id === old) ? old : (rows[0]?.id ?? null),
      );
    } catch (cause) {
      setError(commandErrorMessage(cause));
    } finally {
      setLoading(false);
    }
  }, [scope]);

  useEffect(() => {
    void load();
  }, [load]);

  const selected = useMemo(
    () => presets.find((preset) => preset.id === selectedId) ?? null,
    [presets, selectedId],
  );
  useEffect(() => {
    setRenameValue(selected?.name ?? "");
  }, [selected?.id, selected?.name]);

  const memberIds = new Set(selected?.members.map((member) => member.item_id) ?? []);
  const available = candidates.filter((candidate) => !memberIds.has(candidate.item_id));
  const duplicateName = presets.some(
    (preset) =>
      preset.id !== selected?.id && preset.name.toLowerCase() === renameValue.trim().toLowerCase(),
  );

  async function run(key: string, action: () => Promise<unknown>): Promise<void> {
    setBusy(key);
    setError(null);
    try {
      await action();
      await load();
    } catch (cause) {
      setError(commandErrorMessage(cause));
      toast.warning(commandErrorMessage(cause), { title: "Preset action failed" });
    } finally {
      setBusy(null);
    }
  }

  async function create(): Promise<void> {
    const name = draftName.trim();
    if (!NAME_PATTERN.test(name)) {
      setNameError("Use 1–64 letters, digits, spaces, hyphens or underscores.");
      return;
    }
    if (presets.some((preset) => preset.name.toLowerCase() === name.toLowerCase())) {
      setNameError("A preset with this name already exists in this scope.");
      return;
    }
    setNameError(null);
    setBusy("create");
    try {
      const id = await invoke<string>("preset_create", { name, scope });
      setCreateOpen(false);
      setDraftName("");
      await load();
      setSelectedId(id);
      toast.success(`Preset “${name}” created`);
    } catch (cause) {
      setNameError(commandErrorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  async function apply(preset: PresetView, target: { readonly kind: "agent" | "project"; readonly id: string }): Promise<void> {
    const key = `${preset.id}:${target.kind}:${target.id}`;
    setBusy(key);
    try {
      const result = await invoke<PresetApplyResultView>("preset_apply", {
        request: {
          preset_id: preset.id,
          target_kind: target.kind,
          target_id: target.id,
          project_id: target.kind === "project" ? target.id : activeProject,
        },
      });
      setApplied((current) => ({ ...current, [`${preset.id}:${target.kind}:${target.id}`]: result }));
      await load();
      toast.success(
        result.changed
          ? `${preset.name} applied · ${result.enabled}/${result.total} members on`
          : `${preset.name} was already applied to this target`,
      );
      for (const warning of result.warnings) toast.warning(warning);
    } catch (cause) {
      toast.warning(commandErrorMessage(cause), { title: "Preset action failed" });
      setError(commandErrorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  async function unapply(application: PresetApplicationView): Promise<void> {
    setBusy(`unapply:${application.id}`);
    try {
      const result = await invoke<PresetApplyResultView>("preset_unapply", {
        applicationId: application.id,
      });
      await load();
      toast.success(
        result.changed
          ? `Reverted ${result.changed_members} member${Number(result.changed_members) === 1 ? "" : "s"} · ${result.enabled}/${result.total} still on`
          : "Nothing needed to be reverted",
      );
    } catch (cause) {
      toast.warning(commandErrorMessage(cause), { title: "Preset action failed" });
      setError(commandErrorMessage(cause));
    } finally {
      setBusy(null);
    }
  }

  const projectApplication = (preset: PresetView) =>
    activeProject === null
      ? undefined
      : preset.applications.find(
          (application) => application.target_kind === "project" && application.target === activeProject,
        );

  return (
    <main aria-label="Presets" className="mx-auto flex max-w-[1180px] flex-col gap-20 p-24">
      <header className="flex items-start justify-between gap-16">
        <div>
          <h1 className="m-0 text-22 font-semibold text-text">Presets</h1>
          <p className="m-0 mt-4 text-13 text-text-secondary">
            Reusable sets of skills and MCP servers, applied per agent or per project.
          </p>
        </div>
        <Button variant="primary" onClick={() => { setCreateOpen(true); setNameError(null); }}>
          New preset
        </Button>
      </header>

      {loading ? (
        <p role="status" className="text-13 text-text-secondary">
          Loading presets…
        </p>
      ) : error !== null && presets.length === 0 ? (
        <section role="alert" className="rounded-10 border border-border bg-raised p-24">
          <p className="m-0 text-13 text-danger">{error}</p>
          <Button className="mt-12" onClick={() => void load()}>
            Retry
          </Button>
        </section>
      ) : presets.length === 0 ? (
        <section className="rounded-10 border border-border bg-raised p-24">
          <h2 className="m-0 text-16 font-semibold">No presets yet</h2>
          <p className="mt-8 text-13 text-text-secondary">
            A preset groups skills and MCP servers so one action applies them to an agent or a project.
          </p>
          <Button variant="primary" onClick={() => setCreateOpen(true)}>
            New preset
          </Button>
        </section>
      ) : (
        <div className="grid min-h-[560px] grid-cols-[320px_1fr] gap-16">
          <aside aria-label="Preset list" className="flex flex-col gap-4 rounded-10 border border-border bg-raised p-8">
            {presets.map((preset) => {
              const active = preset.id === selectedId;
              const agentRatios = preset.ratios.filter((ratio) => ratio.total > 0);
              return (
                <button
                  key={preset.id}
                  type="button"
                  aria-pressed={active}
                  aria-label={preset.name}
                  className={`flex w-full flex-col gap-6 rounded-8 border p-12 text-left transition-colors focus-visible:outline-2 focus-visible:outline-focus ${
                    active ? "border-accent bg-overlay" : "border-transparent hover:border-border hover:bg-overlay/60"
                  }`}
                  onClick={() => setSelectedId(preset.id)}
                >
                  <span className="flex items-center justify-between gap-8">
                    <span className="min-w-0 truncate text-14 font-semibold text-text">{preset.name}</span>
                    <span className="shrink-0 text-11 text-text-muted">
                      {preset.members.length} member{preset.members.length === 1 ? "" : "s"}
                    </span>
                  </span>
                  <span className="flex flex-wrap items-center gap-6">
                    {agentRatios.length === 0 ? (
                      <span className="text-11 text-text-muted">Not applied to an agent yet</span>
                    ) : (
                      agentRatios.map((ratio) => (
                        <span
                          key={ratio.target}
                          title={`${ratio.target}: ${ratio.enabled}/${ratio.total} members`}
                          className={`rounded-4 px-6 py-2 font-mono text-11 ${
                            ratio.complete ? "bg-success/15 text-success" : "bg-overlay text-text-secondary"
                          }`}
                        >
                          {ratio.target.replace("_", " ")} {ratio.complete ? "✓" : `${ratio.enabled}/${ratio.total}`}
                        </span>
                      ))
                    )}
                  </span>
                  <span className="text-11 text-text-muted">Created {createdLabel(preset.created_at)}</span>
                </button>
              );
            })}
          </aside>

          <section className="flex min-w-0 flex-col rounded-10 border border-border bg-raised">
            {selected === null ? (
              <p className="p-16 text-13 text-text-secondary">Select a preset.</p>
            ) : (
              <>
                <header className="flex flex-wrap items-center justify-between gap-8 border-b border-border p-12">
                  <div className="flex min-w-0 items-center gap-8">
                    <span className="truncate text-15 font-semibold text-text">{selected.name}</span>
                    <Badge tone="neutral">{selected.scope}</Badge>
                    {selected.missing_members.length > 0 && (
                      <Badge tone="warning" dot>
                        {selected.missing_members.length} missing
                      </Badge>
                    )}
                  </div>
                  <div className="flex items-center gap-8">
                    <Input
                      aria-label="Preset name"
                      value={renameValue}
                      onChange={(event) => setRenameValue(event.target.value)}
                      className="w-192"
                    />
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={busy !== null || renameValue.trim().length === 0 || duplicateName || renameValue.trim() === selected.name}
                      title={duplicateName ? "Another preset already uses this name" : undefined}
                      onClick={() =>
                        void run(`rename:${selected.id}`, () =>
                          invoke("preset_rename", { presetId: selected.id, name: renameValue.trim() }),
                        )
                      }
                    >
                      Rename
                    </Button>
                    <Button
                      size="sm"
                      variant="destructive"
                      disabled={busy !== null}
                      onClick={() => {
                        if (!window.confirm(`Delete the preset “${selected.name}”?`)) return;
                        void run(`delete:${selected.id}`, () =>
                          invoke("preset_delete", { presetId: selected.id }),
                        );
                      }}
                    >
                      Delete
                    </Button>
                  </div>
                </header>
                {duplicateName && (
                  <p role="alert" className="m-0 border-b border-border px-12 py-8 text-12 text-danger">
                    A preset named “{renameValue.trim()}” already exists in this scope.
                  </p>
                )}

                <div className="grid min-h-0 flex-1 grid-cols-[1fr_260px] gap-0">
                  <div className="min-w-0 border-r border-border">
                    <h2 className="m-0 px-12 pt-12 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Members
                    </h2>
                    {selected.members.length === 0 ? (
                      <p className="px-12 py-16 text-13 text-text-muted">
                        This preset has no members yet. Add one from the list on the right.
                      </p>
                    ) : (
                      <ul className="m-0 list-none p-0">
                        {selected.members.map((member) => (
                          <li key={member.item_id} className="flex items-center gap-8 border-t border-border px-12 py-8 first:border-t-0">
                            {member.exists ? (
                              <Toggle
                                size="sm"
                                label={`Enable ${member.name}`}
                                labelPlacement="hidden"
                                checked={member.enabled}
                                disabled={busy !== null}
                                onCheckedChange={(checked) =>
                                  void run(`member:${member.item_id}`, () =>
                                    invoke("preset_member_set", {
                                      presetId: selected.id,
                                      itemId: member.item_id,
                                      kind: member.kind,
                                      enabled: checked,
                                    }),
                                  )
                                }
                              />
                            ) : (
                              <span
                                className="inline-flex size-24 items-center justify-center text-12 text-warning"
                                role="status"
                                aria-label={`${member.name} is missing`}
                              >
                                <span aria-hidden="true">⚠</span>
                              </span>
                            )}
                            <span className="min-w-0 flex-1 truncate text-13 text-text">
                              {member.exists ? member.name : "Deleted item"}
                            </span>
                            <Badge tone={member.kind === "skill" ? "info" : "accent"}>
                              {member.kind === "skill" ? "Skill" : "MCP"}
                            </Badge>
                            <Button
                              size="sm"
                              variant="ghost"
                              disabled={busy !== null}
                              aria-label={`Remove ${member.name}`}
                              onClick={() =>
                                void run(`remove:${member.item_id}`, () =>
                                  invoke("preset_member_set", {
                                    presetId: selected.id,
                                    itemId: member.item_id,
                                    kind: member.kind,
                                    enabled: false,
                                  }),
                                )
                              }
                            >
                              Remove
                            </Button>
                          </li>
                        ))}
                      </ul>
                    )}
                    {selected.missing_members.length > 0 && (
                      <p className="m-0 border-t border-border px-12 py-8 text-12 text-warning">
                        {selected.missing_members.join(", ")} no longer exist{selected.missing_members.length === 1 ? "s" : ""} in
                        the store, so {selected.missing_members.length === 1 ? "it is" : "they are"} excluded from the
                        applied ratios and skipped when applying.
                      </p>
                    )}
                    <h2 className="m-0 px-12 pt-12 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Add member
                    </h2>
                    {available.length === 0 ? (
                      <p className="px-12 py-12 text-13 text-text-muted">
                        Every {scope} skill and MCP server is already in this preset.
                      </p>
                    ) : (
                      <ul className="m-0 list-none p-0">
                        {available.map((candidate) => (
                          <li key={candidate.item_id} className="flex items-center gap-8 border-t border-border px-12 py-6">
                            <span className="min-w-0 flex-1 truncate text-13 text-text-secondary">{candidate.name}</span>
                            <Badge tone={candidate.kind === "skill" ? "info" : "accent"}>
                              {candidate.kind === "skill" ? "Skill" : "MCP"}
                            </Badge>
                            <Button
                              size="sm"
                              variant="secondary"
                              disabled={busy !== null}
                              aria-label={`Add ${candidate.name}`}
                              onClick={() =>
                                void run(`add:${candidate.item_id}`, () =>
                                  invoke("preset_member_set", {
                                    presetId: selected.id,
                                    itemId: candidate.item_id,
                                    kind: candidate.kind,
                                    enabled: true,
                                  }),
                                )
                              }
                            >
                              Add
                            </Button>
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>

                  <div className="flex flex-col gap-12 p-12">
                    <h2 className="m-0 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Apply
                    </h2>
                    {selected.ratios.map((ratio) => {
                      const key = `${selected.id}:agent:${ratio.target}`;
                      const result = applied[key];
                      return (
                        <div key={ratio.target} className="rounded-8 border border-border p-8">
                          <div className="flex items-center justify-between gap-6">
                            <span className="text-13 text-text">{ratio.target.replace("_", " ")}</span>
                            <span className={`font-mono text-11 ${ratio.complete ? "text-success" : "text-text-secondary"}`}>
                              {ratio.complete ? "✓ all on" : `${ratio.enabled}/${ratio.total}`}
                            </span>
                          </div>
                          <Button
                            size="sm"
                            variant="secondary"
                            className="mt-6 w-full"
                            disabled={busy !== null || !selected.applicable}
                            title={
                              selected.applicable
                                ? undefined
                                : "This preset has no existing enabled members to apply"
                            }
                            onClick={() => void apply(selected, { kind: "agent", id: ratio.target })}
                          >
                            {busy === `${selected.id}:agent:${ratio.target}`
                              ? "Applying…"
                              : result?.changed
                                ? "Applied ✓"
                                : ratio.complete
                                  ? "Re-apply"
                                  : "Apply to agent"}
                          </Button>
                        </div>
                      );
                    })}
                    {activeProject !== null && (
                      <div className="rounded-8 border border-border p-8">
                        <div className="flex items-center justify-between gap-6">
                          <span className="text-13 text-text">This project</span>
                          <span className="font-mono text-11 text-text-muted">{activeProject}</span>
                        </div>
                        <Button
                          size="sm"
                          variant="secondary"
                          className="mt-6 w-full"
                          disabled={busy !== null || !selected.applicable}
                          title={
                            selected.applicable
                              ? undefined
                              : "This preset has no existing enabled members to apply"
                          }
                          onClick={() => void apply(selected, { kind: "project", id: activeProject })}
                        >
                          {busy === `${selected.id}:project:${activeProject}` ? "Applying…" : "Apply to project"}
                        </Button>
                      </div>
                    )}
                    <h2 className="m-0 mt-8 text-11 font-semibold uppercase tracking-wide text-text-muted">
                      Applied history
                    </h2>
                    {selected.applications.length === 0 ? (
                      <p className="m-0 text-12 text-text-muted">Never applied.</p>
                    ) : (
                      <ul className="m-0 flex list-none flex-col gap-6 p-0">
                        {selected.applications.map((application) => (
                          <li key={application.id} className="rounded-8 border border-border p-8">
                            <div className="flex items-center justify-between gap-6 text-12">
                              <span className="text-text">
                                {application.target.replace("_", " ")} · {application.member_count} member
                                {Number(application.member_count) === 1 ? "" : "s"}
                              </span>
                              <span className="text-text-muted">{createdLabel(application.applied_at)}</span>
                            </div>
                            <Button
                              size="sm"
                              variant="ghost"
                              className="mt-4 w-full"
                              disabled={busy !== null}
                              onClick={() => void unapply(application)}
                            >
                              {busy === `unapply:${application.id}` ? "Reverting…" : "Unapply"}
                            </Button>
                          </li>
                        ))}
                      </ul>
                    )}
                    {projectApplication(selected) === undefined && activeProject !== null && (
                      <p className="m-0 text-11 text-text-muted">
                        Applying to a project rewrites that project's native files for every managed agent.
                      </p>
                    )}
                  </div>
                </div>
              </>
            )}
          </section>
        </div>
      )}

      <Modal
        open={createOpen}
        title="New preset"
        description="Presets group skills and MCP servers so one action applies them together."
        onOpenChange={(open) => setCreateOpen(open)}
        footer={
          <>
            <Button variant="ghost" onClick={() => setCreateOpen(false)}>
              Cancel
            </Button>
            <Button variant="primary" disabled={busy === "create"} onClick={() => void create()}>
              {busy === "create" ? "Creating…" : "Create preset"}
            </Button>
          </>
        }
      >
        <Input
          label="Preset name"
          value={draftName}
          onChange={(event) => {
            setDraftName(event.target.value);
            setNameError(null);
          }}
          error={nameError ?? undefined}
          autoFocus
        />
      </Modal>
    </main>
  );
}
