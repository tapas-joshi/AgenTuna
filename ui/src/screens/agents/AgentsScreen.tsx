import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentCardView } from "../../generated/AgentCardView";
import { Button, SegmentedControl, Toggle } from "../../components";
import { ConnectAgentDialog } from "../../overlays/dialogs/ConnectAgentDialog";
import { CustomAgentDialog } from "../../overlays/dialogs/CustomAgentDialog";
import { useToast } from "../../overlays/toasts";
import { useAppState } from "../../state/context";
import { shellAgentStatus } from "../../shell/agents";
import { commandErrorMessage } from "../../lib/commandError";

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
        <div className="size-[44px] rounded-8 bg-overlay" />
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
  const app = useAppState();
  const [agents, setAgents] = useState<readonly AgentCardView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [rescanError, setRescanError] = useState<string | null>(null);
  const [pathAgent, setPathAgent] = useState<AgentCardView | null>(null);
  const [customAgentOpen, setCustomAgentOpen] = useState(false);
  const pathRequest = useRef<string | null>(null);

  /** Stores the screen's rows and feeds the same detections to the sidebar. */
  const publish = useCallback(
    (cards: readonly AgentCardView[]) => {
      setAgents(cards);
      app.receiveAgents(shellAgentStatus(cards));
    },
    [app],
  );

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      publish(await invoke<AgentCardView[]>("agents_list"));
    } catch (cause) {
      setError(commandErrorMessage(cause));
    } finally {
      setLoading(false);
    }
  }, [publish]);

  useEffect(() => {
    void load();
  }, [load]);

  // "Set path" from Overview selects an agent; open the same path dialog the
  // card menu opens, once per requested id.
  useEffect(() => {
    const requested = app.getSnapshot().selections.agents?.[0];
    if (requested === undefined || pathRequest.current === requested || agents.length === 0) return;
    const target = agents.find((agent) => agent.id === requested);
    if (target === undefined) return;
    pathRequest.current = requested;
    setPathAgent(target);
  }, [agents, app]);

  async function rescan(): Promise<void> {
    setBusy("rescan");
    setRescanError(null);
    try {
      publish(await invoke<AgentCardView[]>("agents_rescan"));
    } catch (cause) {
      setRescanError(commandErrorMessage(cause));
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
      publish(next);
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
      setError(commandErrorMessage(cause));
      toast.warning(commandErrorMessage(cause), { title: "Agent change failed" });
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
      publish(refreshed);
      if (mode === "copy" && agent.linked_deployments > 0) {
        toast.info(
          `${agent.linked_deployments} existing link${agent.linked_deployments === 1 ? "" : "s"} will migrate to copies on the next deploy.`,
        );
      }
    } catch (cause) {
      setError(commandErrorMessage(cause));
      toast.warning(commandErrorMessage(cause), { title: "Agent change failed" });
    } finally {
      setBusy(null);
    }
  }



  return (
    <main aria-label="Agents" className="mx-auto flex max-w-[1180px] flex-col gap-20 p-24 max-[1199px]:gap-24">
      <header className="flex items-start justify-between gap-16">
        <div>
          <h1 className="m-0 text-22 font-semibold text-text">Agents</h1>
          <p className="m-0 mt-4 text-13 text-text-secondary">
            Coding agents found on this Mac. AgenTuna writes each one's native config format.
          </p>
        </div>
        <div className="flex gap-8">
          <Button variant="secondary" onClick={() => setCustomAgentOpen(true)}>Add custom agent</Button>
          <Button variant="secondary" aria-label="Rescan all agents" disabled={busy !== null} onClick={() => void rescan()}>
            {busy === "rescan" ? "Rescanning…" : "Rescan"}
          </Button>
        </div>
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
                id={`agent-card-${agent.id}`}
                tabIndex={-1}
                key={agent.id}
                aria-label={agent.name}
                className="flex flex-col gap-12 rounded-10 border border-border bg-raised p-16"
              >
                <header className="flex items-start gap-12">
                  <span
                    aria-hidden="true"
                    className={`grid size-[44px] shrink-0 place-items-center rounded-8 text-13 font-semibold ${
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
                    <dd className="m-0 min-w-0 font-mono text-12 text-text-secondary">
                      {agent.paths.length > 0
                        ? agent.paths.map((path) => <span key={path} className="block truncate" title={path}>{path}</span>)
                        : "not found"}
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
                {agent.managed && agent.total === 0 ? (
                  <p className="m-0 flex flex-wrap items-baseline gap-6 text-11 text-text-muted">
                    <span className="break-words font-mono">{agent.primary_path ?? "<path>"}</span>
                    <span>· Nothing deployed yet</span>
                  </p>
                ) : (
                  <p className="m-0 break-words font-mono text-11 text-text-muted">
                    {agent.managed ? agent.health : "Not managed by AgenTuna"}
                  </p>
                )}

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
                        }}
                      >
                        Set path…
                      </Button>
                    </div>
                  ) : (
                    <div className="flex flex-wrap items-center justify-between gap-12">
                      <Toggle
                        label="Let AgenTuna manage this agent"
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

      <ConnectAgentDialog
        agent={pathAgent}
        onClose={() => setPathAgent(null)}
        onSaved={(updated) => {
          publish(agents.map((row) => (row.id === updated.id ? updated : row)));
          setPathAgent(null);
        }}
      />
      <CustomAgentDialog
        open={customAgentOpen}
        onClose={() => setCustomAgentOpen(false)}
        onCreated={(created) => {
          publish(agents.some((row) => row.id === created.id)
            ? agents.map((row) => (row.id === created.id ? created : row))
            : [...agents, created]);
        }}
        onOpenExisting={(agentId) => {
          setCustomAgentOpen(false);
          const openCard = () => {
            const card = document.getElementById(`agent-card-${agentId}`);
            card?.scrollIntoView({ behavior: "smooth", block: "center" });
            card?.focus({ preventScroll: true });
          };
          if (agents.some((agent) => agent.id === agentId)) {
            window.requestAnimationFrame(openCard);
          } else {
            void invoke<AgentCardView[]>("agents_list").then((next) => {
              setAgents(next);
              window.requestAnimationFrame(openCard);
            }).catch((cause: unknown) => setError(commandErrorMessage(cause)));
          }
        }}
      />
    </main>
  );
}
