import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, Input, SegmentedControl, Toggle } from "../../components";
import type { AgentCardView } from "../../generated/AgentCardView";
import { Modal } from "../../overlays";
import { useToast } from "../../overlays/toasts";

const INITIALS: Readonly<Record<string, string>> = {
  claude_code: "CL",
  codex: "CO",
  omp: "OM",
  cursor: "CU",
};

function presenceLabel(agent: AgentCardView): string {
  if (agent.presence === "installed") return "Installed";
  if (agent.presence === "configured_only") return "Found config, CLI missing";
  return "Not installed";
}

function outsideAgenTuna(agent: AgentCardView): string {
  const parts: string[] = [];
  if (agent.native_skills > 0) parts.push(`${agent.native_skills} skill${agent.native_skills === 1 ? "" : "s"}`);
  if (agent.native_servers > 0) parts.push(`${agent.native_servers} server${agent.native_servers === 1 ? "" : "s"}`);
  if (parts.length === 0) return "Nothing outside AgenTuna";
  return `+${parts.join(", ")} outside AgenTuna`;
}

/** Skeleton card shown while detection is in flight. */
function SkeletonCard() {
  return (
    <article aria-hidden="true" className="animate-pulse rounded-10 border border-border bg-raised p-16">
      <div className="flex items-center gap-12">
        <div className="size-40 rounded-8 bg-overlay" />
        <div className="flex-1">
          <div className="h-14 w-1/2 rounded-4 bg-overlay" />
          <div className="mt-6 h-10 w-1/3 rounded-4 bg-overlay" />
        </div>
      </div>
      <div className="mt-16 h-10 w-2/3 rounded-4 bg-overlay" />
      <div className="mt-12 h-10 w-1/2 rounded-4 bg-overlay" />
      <div className="mt-20 h-24 w-full rounded-4 bg-overlay" />
    </article>
  );
}

/** Agent inventory with health, management, deployment mode and path repair. */
export function AgentsScreen() {
  const toast = useToast();
  const [agents, setAgents] = useState<readonly AgentCardView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [rescanError, setRescanError] = useState<string | null>(null);
  const [pathAgent, setPathAgent] = useState<AgentCardView | null>(null);
  const [pathValue, setPathValue] = useState("");
  const [pathError, setPathError] = useState<string | null>(null);
  const [pathBusy, setPathBusy] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setAgents(await invoke<AgentCardView[]>("agents_list"));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function rescan(): Promise<void> {
    setBusy("rescan");
    setRescanError(null);
    try {
      setAgents(await invoke<AgentCardView[]>("agents_rescan"));
    } catch (cause) {
      setRescanError(String(cause));
    } finally {
      setBusy(null);
    }
  }

  async function setManaged(agent: AgentCardView, managed: boolean): Promise<void> {
    setBusy(`manage:${agent.id}`);
    setError(null);
    try {
      await invoke("agent_manage_set", { agentId: agent.id, managed });
      const next = await invoke<AgentCardView[]>("agents_rescan");
      setAgents(next);
      const refreshed = next.find((row) => row.id === agent.id);
      if (managed) {
        toast.success(
          `AgenTuna now manages ${agent.name} · ${refreshed?.total ?? 0} deployment${(refreshed?.total ?? 0) === 1 ? "" : "s"} affected`,
        );
      } else {
        toast.info(
          `AgenTuna stopped managing ${agent.name}. Files already on disk were left in place.`,
        );
      }
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Agent change failed" });
    } finally {
      setBusy(null);
    }
  }

  async function setMode(agent: AgentCardView, mode: "link" | "copy"): Promise<void> {
    setBusy(`mode:${agent.id}`);
    setError(null);
    try {
      await invoke("agent_link_mode_set", { agentId: agent.id, mode });
      const refreshed = await invoke<AgentCardView[]>("agents_list");
      setAgents(refreshed);
      if (mode === "copy" && agent.linked_deployments > 0) {
        toast.info(
          `${agent.linked_deployments} existing link${agent.linked_deployments === 1 ? "" : "s"} will migrate to copies on the next deploy.`,
        );
      }
    } catch (cause) {
      setError(String(cause));
      toast.warning(String(cause), { title: "Agent change failed" });
    } finally {
      setBusy(null);
    }
  }

  async function choosePath(): Promise<void> {
    try {
      const selected = await invoke<string | null>("agent_path_choose", {
        title: `Choose the ${pathAgent?.name ?? "agent"} configuration folder`,
      });
      if (selected !== null) setPathValue(selected);
    } catch (cause) {
      setPathError(String(cause));
    }
  }

  async function savePath(): Promise<void> {
    if (pathAgent === null) return;
    setPathBusy(true);
    setPathError(null);
    try {
      const updated = await invoke<AgentCardView>("agent_path_set", {
        agentId: pathAgent.id,
        path: pathValue,
      });
      setAgents((current) => current.map((row) => (row.id === updated.id ? updated : row)));
      setPathAgent(null);
      toast.success(`${updated.name} now points at ${updated.primary_path ?? pathValue}`);
    } catch (cause) {
      setPathError(String(cause));
    } finally {
      setPathBusy(false);
    }
  }

  return (
    <main aria-label="Agents" className="mx-auto flex max-w-[1180px] flex-col gap-20 p-24">
      <header className="flex items-start justify-between gap-16">
        <div>
          <h1 className="m-0 text-22 font-semibold text-text">Agents</h1>
          <p className="m-0 mt-4 text-13 text-text-secondary">
            Coding agents found on this Mac. AgenTuna writes each one's native config format.
          </p>
        </div>
        <Button variant="secondary" aria-label="Rescan all agents" disabled={busy !== null} onClick={() => void rescan()}>
          {busy === "rescan" ? "Rescanning…" : "Rescan"}
        </Button>
      </header>

      {rescanError !== null && (
        <div role="alert" className="flex items-center justify-between gap-12 rounded-8 border border-danger/40 bg-danger/10 p-12">
          <p className="m-0 text-13 text-danger">Rescan failed: {rescanError}</p>
          <Button size="sm" onClick={() => void rescan()}>
            Retry
          </Button>
        </div>
      )}
      {error !== null && (
        <p role="alert" className="m-0 text-13 text-danger">
          {error}
        </p>
      )}

      {loading ? (
        <div className="grid gap-16 md:grid-cols-2" aria-label="Detecting agents" role="status">
          <SkeletonCard />
          <SkeletonCard />
        </div>
      ) : agents.length === 0 ? (
        <section className="rounded-10 border border-border bg-raised p-24">
          <h2 className="m-0 text-16 font-semibold">No agents detected</h2>
          <p className="mt-8 text-13 text-text-secondary">
            Install an agent, then rescan to let AgenTuna find its configuration.
          </p>
          <Button onClick={() => void rescan()}>Rescan</Button>
        </section>
      ) : (
        <div className="grid gap-16 md:grid-cols-2">
          {agents.map((agent) => {
            const missing = agent.presence === "not_installed";
            return (
              <article
                key={agent.id}
                aria-label={agent.name}
                className="flex flex-col gap-12 rounded-10 border border-border bg-raised p-16"
              >
                <header className="flex items-start gap-12">
                  <span
                    aria-hidden="true"
                    className={`grid h-40 w-40 shrink-0 place-items-center rounded-8 text-13 font-semibold ${
                      missing ? "bg-overlay text-text-muted" : "bg-overlay text-text"
                    }`}
                  >
                    {INITIALS[agent.id] ?? agent.name.slice(0, 2).toUpperCase()}
                  </span>
                  <div className="min-w-0 flex-1">
                    <h2 className="m-0 text-16 font-semibold text-text">{agent.name}</h2>
                    <p className="m-0 text-12 text-text-secondary">
                      {presenceLabel(agent)}
                      {agent.version !== null && agent.presence === "installed" ? ` · ${agent.version}` : ""}
                    </p>
                  </div>
                  <span className="inline-flex items-center gap-6 text-12 text-text-secondary">
                    <span
                      aria-hidden="true"
                      className={`size-8 rounded-full ${missing ? "bg-text-muted" : "bg-success"}`}
                    />
                    {missing ? "Missing" : "Detected"}
                  </span>
                </header>

                <dl className="m-0 grid grid-cols-2 gap-x-16 gap-y-8 text-12">
                  <div className="min-w-0">
                    <dt className="text-11 uppercase tracking-wide text-text-muted">Config</dt>
                    <dd className="m-0 truncate font-mono text-12 text-text-secondary" title={agent.paths.join("  ·  ")}>
                      {agent.paths.length > 0 ? agent.paths.join("  ·  ") : "not found"}
                    </dd>
                  </div>
                  <div className="min-w-0">
                    <dt className="text-11 uppercase tracking-wide text-text-muted">Format</dt>
                    <dd className="m-0 truncate text-12 text-text-secondary">{agent.format}</dd>
                  </div>
                  <div className="min-w-0">
                    <dt className="text-11 uppercase tracking-wide text-text-muted">Managed</dt>
                    <dd className="m-0 text-12 text-text-secondary">
                      {agent.managed
                        ? `${agent.managed_skills} skill${agent.managed_skills === 1 ? "" : "s"} · ${agent.managed_servers} server${agent.managed_servers === 1 ? "" : "s"}`
                        : "Not managed by AgenTuna"}
                    </dd>
                    {agent.managed && (
                      <dd className={`m-0 text-11 ${agent.native_skills + agent.native_servers > 0 ? "text-warning" : "text-text-muted"}`}>
                        {outsideAgenTuna(agent)}
                      </dd>
                    )}
                  </div>
                </dl>
                <p className="m-0 break-words font-mono text-11 text-text-muted">
                  {agent.managed ? agent.health : "Not managed by AgenTuna"}
                </p>

                <div className="mt-auto border-t border-border pt-12">
                  {missing ? (
                    <div className="flex items-center justify-between gap-12">
                      <p className="m-0 text-12 text-text-secondary">
                        Point AgenTuna at the app to finish setup.
                      </p>
                      <Button
                        size="sm"
                        onClick={() => {
                          setPathAgent(agent);
                          setPathValue(agent.primary_path ?? "");
                          setPathError(null);
                        }}
                      >
                        Set path…
                      </Button>
                    </div>
                  ) : (
                    <div className="flex flex-wrap items-center justify-between gap-12">
                      <Toggle
                        label={`Manage ${agent.name}`}
                        labelPlacement="inline"
                        checked={agent.managed}
                        disabled={busy === `manage:${agent.id}`}
                        onCheckedChange={(checked) => void setManaged(agent, checked)}
                      />
                      <div className="flex items-center gap-8">
                        <SegmentedControl
                          label={`${agent.name} deployment mode`}
                          value={agent.link_mode === "copy" ? "copy" : "link"}
                          onValueChange={(mode) => {
                            if (mode !== agent.link_mode) void setMode(agent, mode === "copy" ? "copy" : "link");
                          }}
                          options={[
                            { value: "link", label: "Link" },
                            { value: "copy", label: "Copy" },
                          ]}
                          size="md"
                        />
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy !== null}
                          onClick={() => void rescan()}
                        >
                          Rescan
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      )}

      <Modal
        open={pathAgent !== null}
        title={`Set path for ${pathAgent?.name ?? "agent"}`}
        description="Choose the folder or document AgenTuna should read for this agent. The path is verified before it is saved."
        onOpenChange={(open) => {
          if (!open) setPathAgent(null);
        }}
        footer={
          <>
            <Button variant="ghost" onClick={() => setPathAgent(null)}>
              Cancel
            </Button>
            <Button variant="secondary" onClick={() => void choosePath()}>
              Choose folder…
            </Button>
            <Button
              variant="primary"
              disabled={pathBusy || pathValue.trim().length === 0}
              onClick={() => void savePath()}
            >
              {pathBusy ? "Saving…" : "Save path"}
            </Button>
          </>
        }
      >
        <label className="grid gap-6 text-13">
          Configuration path
          <Input
            value={pathValue}
            onChange={(event) => setPathValue(event.target.value)}
            placeholder="/Users/you/.cursor/skills"
            aria-label="Agent configuration path"
          />
        </label>
        {pathError !== null && (
          <p role="alert" className="m-0 mt-8 text-13 text-danger">
            {pathError}
          </p>
        )}
      </Modal>
    </main>
  );
}
