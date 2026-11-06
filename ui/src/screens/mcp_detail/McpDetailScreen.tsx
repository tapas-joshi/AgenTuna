/**
 * MCP server detail screen (TM-021).
 *
 * Detailed view with configuration, monospace command/args/env, secret Keychain account chips,
 * 5-slot health history strip, cadence text, contextual Fix token / Sign in actions (TM-026),
 * per-agent Loaded by toggles with immediate state updates, and Why trace dialog.
 */

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, Badge } from "../../components";
import type { McpServerDetailView } from "../../generated/McpServerDetailView";
import type { McpServerDetailRequest } from "../../generated/McpServerDetailRequest";
import type { McpServerActionRequest } from "../../generated/McpServerActionRequest";
import type { McpServerActionOutcome } from "../../generated/McpServerActionOutcome";
import type { McpServerTestView } from "../../generated/McpServerTestView";
import { WhyTraceDialog } from "../../overlays";
import { FixTokenDialog } from "../secrets/FixTokenDialog";
import { useToast } from "../../overlays/toasts";
import { useAppSnapshot, useAppState } from "../../state/context";

const SCOPE_BADGE: Readonly<Record<string, string>> = {
  global: "bg-scope-global/15 text-scope-global",
  user: "bg-scope-user/15 text-scope-user",
  project: "bg-scope-project/15 text-scope-project",
  local: "bg-scope-project/15 text-scope-project",
};

function parseCommandError(cause: unknown): { code: string; message: string } {
  if (typeof cause === "object" && cause !== null && "code" in cause && "message" in cause) {
    const value = cause as { code: unknown; message: unknown };
    return { code: String(value.code), message: String(value.message) };
  }
  return { code: "error", message: String(cause) };
}

export function McpDetailScreen() {
  const snapshot = useAppSnapshot();
  const { scope, activeProject } = snapshot;
  const app = useAppState();
  const toast = useToast();

  const selectedId =
    snapshot.selections["mcp-detail"]?.[0] ??
    (typeof snapshot.screenState["detail:itemId"] === "string"
      ? (snapshot.screenState["detail:itemId"] as string)
      : "");

  const [detail, setDetail] = useState<McpServerDetailView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [testing, setTesting] = useState(false);
  const [testAbort, setTestAbort] = useState(false);
  const [expandedTools, setExpandedTools] = useState(false);
  const [showWhy, setShowWhy] = useState(false);
  const [showFixToken, setShowFixToken] = useState(false);

  const load = useCallback(async () => {
    if (!selectedId) {
      setError("No MCP server selected");
      setLoading(false);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const req: McpServerDetailRequest = {
        item_id: selectedId,
        scope,
        project_id: activeProject,
      };
      const result = await invoke<McpServerDetailView>("mcp_server_detail", { request: req });
      setDetail(result);
    } catch (cause) {
      setError(parseCommandError(cause).message);
    } finally {
      setLoading(false);
    }
  }, [selectedId, scope, activeProject]);

  useEffect(() => {
    void load();
  }, [load]);

  const handleTestConnection = useCallback(async () => {
    if (!detail) return;
    setTesting(true);
    setTestAbort(false);
    try {
      const result = await invoke<McpServerTestView>("mcp_server_test", {
        request: { item_id: detail.item_id, scope: detail.scope, project_id: activeProject },
      });
      if (!testAbort) {
        toast.success(
          `${result.server_name}: ${result.tools_count} tool(s) in ${result.latency_ms} ms`,
          { title: "Connection OK" }
        );
        await load();
      }
    } catch (cause) {
      if (!testAbort) {
        const failure = parseCommandError(cause);
        toast.warning(failure.message, { title: "Test failed" });
      }
    } finally {
      setTesting(false);
    }
  }, [detail, activeProject, testAbort, toast, load]);

  const handleToggleAgent = useCallback(
    async (agentId: string, currentEnabled: boolean) => {
      if (!detail) return;
      const next = !currentEnabled;
      // Optimistically update within one render after engine reply
      const prevAgents = detail.agents;
      const updatedAgents = detail.agents.map((a) =>
        a.id === agentId ? { ...a, enabled: next, state: next ? "linked" : "off" } : a
      );
      setDetail({ ...detail, agents: updatedAgents });

      const command = next ? "mcp_server_enable" : "mcp_server_disable";
      const baseReq: McpServerActionRequest = {
        item_id: detail.item_id,
        scope: detail.scope,
        project_id: activeProject,
        agent: agentId,
        in_repo: false,
        apply: false,
        preview_id: null,
      };

      try {
        const preview = await invoke<McpServerActionOutcome>(command, {
          request: baseReq,
        });
        if (preview.stage === "preview") {
          await invoke<McpServerActionOutcome>(command, {
            request: { ...baseReq, apply: true, preview_id: preview.preview.preview_id },
          });
        }
        toast.success(
          next
            ? `Enabled ${detail.name} for ${agentId}`
            : `Disabled ${detail.name} for ${agentId}`
        );
        await load();
      } catch (cause) {
        // Revert on failure with a toast
        setDetail((d) => (d ? { ...d, agents: prevAgents } : null));
        const failure = parseCommandError(cause);
        toast.warning(failure.message, { title: "Agent toggle failed" });
      }
    },
    [detail, activeProject, toast, load]
  );

  const handleDisableInRepo = useCallback(async () => {
    if (!detail) return;
    const baseReq: McpServerActionRequest = {
      item_id: detail.item_id,
      scope: detail.scope,
      project_id: activeProject,
      agent: null,
      in_repo: true,
      apply: false,
      preview_id: null,
    };
    try {
      const preview = await invoke<McpServerActionOutcome>("mcp_server_disable", {
        request: baseReq,
      });
      if (preview.stage === "preview") {
        await invoke<McpServerActionOutcome>("mcp_server_disable", {
          request: { ...baseReq, apply: true, preview_id: preview.preview.preview_id },
        });
      }
      toast.success(`Disabled ${detail.name} in this repository`);
      await load();
    } catch (cause) {
      toast.warning(parseCommandError(cause).message, { title: "Disable in repo failed" });
    }
  }, [detail, activeProject, toast, load]);

  if (loading && detail === null) {
    return (
      <main aria-label="MCP Server Detail" className="mx-auto max-w-[1100px] p-24">
        <p role="status" className="text-13 text-text-muted">Loading server details…</p>
      </main>
    );
  }

  if (error !== null || detail === null) {
    return (
      <main aria-label="MCP Server Detail" className="mx-auto max-w-[1100px] p-24">
        <div role="alert" className="flex flex-col items-start gap-12 rounded-8 border border-danger/30 bg-danger/10 p-20 text-text">
          <h2 className="m-0 text-16 font-semibold">Server not found</h2>
          <p className="m-0 text-13 text-text-secondary">
            {error ?? "This MCP server definition is missing or was removed externally."}
          </p>
          <Button onClick={() => app.navigate("mcp-list")}>Back to MCP Servers</Button>
        </div>
      </main>
    );
  }

  // 5 slots, oldest last
  const healthSlots = [...detail.last_checks].slice(0, 5);
  while (healthSlots.length < 5) {
    healthSlots.push({
      checked_at: "",
      status: "none",
      latency_ms: null,
      trigger: "",
      error_text: null,
    });
  }

  const toolsList = detail.tools;
  const visibleTools = expandedTools ? toolsList : toolsList.slice(0, 5);

  return (
    <main aria-label="MCP Server Detail" className="mx-auto flex max-w-[1100px] flex-col gap-24 p-24">
      {/* Header */}
      <header className="flex flex-wrap items-center justify-between gap-16 border-b border-border pb-16">
        <div className="flex flex-col gap-6">
          <div className="flex flex-wrap items-center gap-10">
            <h1 className="m-0 text-22 font-semibold text-text">{detail.name}</h1>
            <span
              className={`inline-flex items-center rounded-4 px-8 py-2 text-11 font-medium capitalize ${
                SCOPE_BADGE[detail.scope] ?? "bg-surface text-text-secondary"
              }`}
            >
              {detail.scope}
            </span>
            {detail.locked && (
              <span className="text-12 text-scope-global" title={detail.lock_reason ?? "Locked"}>
                🔒 Locked
              </span>
            )}
          </div>
          {detail.description && (
            <p className="m-0 text-13 text-text-muted">{detail.description}</p>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-8">
          <Button variant="ghost" onClick={() => void handleDisableInRepo()}>
            Disable in this repo
          </Button>
          <Button
            variant="secondary"
            disabled={testing}
            onClick={() => void handleTestConnection()}
          >
            {testing ? "Testing…" : "Test connection"}
          </Button>
          {testing && (
            <Button size="sm" variant="ghost" onClick={() => setTestAbort(true)}>
              Cancel
            </Button>
          )}
          <Button
            onClick={() => {
              app.setSelection("mcp-form", [detail.item_id]);
              app.navigate("mcp-form");
            }}
          >
            Edit
          </Button>
        </div>
      </header>

      {/* Two columns */}
      <div className="grid grid-cols-1 gap-24 md:grid-cols-2">
        {/* Left Column: Configuration, Tools, State */}
        <div className="flex flex-col gap-20">
          <section className="flex flex-col gap-12 rounded-8 border border-border bg-surface p-16">
            <h2 className="m-0 text-15 font-semibold text-text">Configuration</h2>
            <div className="flex flex-col gap-8 text-13">
              <div>
                <span className="text-text-muted">Transport: </span>
                <span className="font-mono font-medium text-text">{detail.transport}</span>
              </div>

              {detail.defined_in.length > 0 && (
                <div className="flex flex-col gap-4">
                  <span className="text-text-muted">Defined in:</span>
                  <div className="flex flex-col gap-2 pl-8">
                    {detail.defined_in.map((p) => (
                      <span key={p} className="font-mono text-11 text-text-secondary truncate" title={p}>
                        {p}
                      </span>
                    ))}
                  </div>
                </div>
              )}

              {detail.command && (
                <div>
                  <span className="text-text-muted">Command: </span>
                  <span className="font-mono text-12 text-text">{detail.command}</span>
                </div>
              )}

              {detail.args.length > 0 && (
                <div>
                  <span className="text-text-muted">Args: </span>
                  <span className="font-mono text-12 text-text">{detail.args.join(" ")}</span>
                </div>
              )}

              {detail.url && (
                <div>
                  <span className="text-text-muted">URL: </span>
                  <span className="font-mono text-12 text-text">{detail.url}</span>
                </div>
              )}

              {detail.cwd && (
                <div>
                  <span className="text-text-muted">Working dir: </span>
                  <span className="font-mono text-12 text-text">{detail.cwd}</span>
                </div>
              )}
            </div>

            {/* Environment Variables */}
            {detail.env.length > 0 && (
              <div className="flex flex-col gap-6 pt-8 border-t border-border">
                <span className="text-12 font-medium text-text-secondary">Environment</span>
                <div className="flex flex-col gap-4">
                  {detail.env.map((row) => (
                    <div
                      key={row.key}
                      className="flex flex-wrap items-center justify-between gap-8 rounded-4 bg-raised px-8 py-4 font-mono text-12"
                    >
                      <span className="font-medium text-text">{row.key}</span>
                      {row.is_secret ? (
                        <Badge tone="info" mono>
                          Keychain: {row.keychain_account ?? "configured"}
                        </Badge>
                      ) : (
                        <span className="text-text-muted">{row.value}</span>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            )}
          </section>

          {/* Tools */}
          <section className="flex flex-col gap-10 rounded-8 border border-border bg-surface p-16">
            <h2 className="m-0 text-15 font-semibold text-text">
              Tools {toolsList.length > 0 && `(${toolsList.length})`}
            </h2>
            {toolsList.length === 0 ? (
              <p className="m-0 text-12 text-text-muted">
                No tools listed yet. Run "Test connection" to inspect advertised tools.
              </p>
            ) : (
              <div className="flex flex-wrap gap-6">
                {visibleTools.map((tool) => (
                  <Badge key={tool} tone="muted" mono>
                    {tool}
                  </Badge>
                ))}
                {!expandedTools && toolsList.length > 5 && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setExpandedTools(true)}
                  >
                    +{toolsList.length - 5} more
                  </Button>
                )}
              </div>
            )}
          </section>
        </div>

        {/* Right Column: Health, Loaded By, Why trace */}
        <div className="flex flex-col gap-20">
          {/* Health */}
          <section className="flex flex-col gap-12 rounded-8 border border-border bg-surface p-16">
            <div className="flex items-center justify-between">
              <h2 className="m-0 text-15 font-semibold text-text">Health</h2>
              <span className="text-11 text-text-muted">{detail.cadence_text}</span>
            </div>

            <div className="flex items-center gap-8">
              <span className="text-13 text-text-muted">Status:</span>
              <span
                className={`font-semibold capitalize text-13 ${
                  detail.health_status === "ok"
                    ? "text-success"
                    : detail.health_status === "failed"
                      ? "text-danger"
                      : "text-text-muted"
                }`}
              >
                {detail.health_status === "not_tested" ? "Not tested" : detail.health_status}
              </span>
            </div>

            {/* Exactly 5 slots, oldest last */}
            <div className="flex flex-col gap-4">
              <span className="text-11 text-text-muted">Last 5 checks:</span>
              <div
                className="flex items-center gap-6"
                role="group"
                aria-label="Health check history slots"
              >
                {detail.last_checks.length === 0 ? (
                  <span className="text-12 text-text-muted">Not tested</span>
                ) : (
                  healthSlots.map((slot, index) => {
                    const ok = slot.status === "ok";
                    const failed = slot.status === "failed";
                    const isBlank = slot.status === "none";

                    const color = isBlank
                      ? "bg-border/40"
                      : ok
                        ? "bg-success"
                        : failed
                          ? "bg-danger"
                          : "bg-warning";

                    const tip = isBlank
                      ? "No record"
                      : `${slot.status} · ${slot.trigger}${slot.latency_ms ? ` · ${slot.latency_ms} ms` : ""}${slot.error_text ? ` · ${slot.error_text}` : ""}`;

                    return (
                      <div
                        key={index}
                        title={tip}
                        aria-label={`Check ${index + 1}: ${slot.status}`}
                        className={`h-16 w-16 rounded-3 ${color}`}
                      />
                    );
                  })
                )}
              </div>
            </div>

            {/* Contextual button: Fix token or Sign in */}
            {detail.needs_auth && (
              <div className="pt-8 border-t border-border">
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => setShowFixToken(true)}
                >
                  {detail.is_oauth ? "Sign in" : "Fix token"}
                </Button>
              </div>
            )}
          </section>

          {/* Loaded By */}
          <section className="flex flex-col gap-12 rounded-8 border border-border bg-surface p-16">
            <h2 className="m-0 text-15 font-semibold text-text">Loaded by</h2>
            <div className="flex flex-col gap-8">
              {detail.agents.map((agent) => {
                const isNotInstalled = agent.presence === "not_installed";

                return (
                  <div
                    key={agent.id}
                    className={`flex items-center justify-between gap-12 rounded-6 border border-border p-8 ${
                      isNotInstalled ? "bg-raised/50 opacity-60" : "bg-raised"
                    }`}
                  >
                    <div className="flex flex-col gap-2">
                      <span className="font-medium text-13 text-text">{agent.label}</span>
                      {isNotInstalled && agent.reason && (
                        <span className="text-11 text-text-muted">{agent.reason}</span>
                      )}
                      {!isNotInstalled && (
                        <span className="text-11 text-text-secondary capitalize">{agent.state}</span>
                      )}
                    </div>

                    <button
                      type="button"
                      role="switch"
                      aria-checked={agent.enabled}
                      aria-label={`${agent.label}: ${agent.enabled ? "enabled" : "disabled"}`}
                      disabled={isNotInstalled || detail.locked}
                      onClick={() => void handleToggleAgent(agent.id, agent.enabled)}
                      className={`h-20 w-36 rounded-full transition-colors relative ${
                        agent.enabled ? "bg-accent" : "bg-border"
                      } disabled:cursor-not-allowed`}
                    >
                      <span
                        className={`absolute top-2 h-16 w-16 rounded-full bg-white transition-transform ${
                          agent.enabled ? "left-18" : "left-2"
                        }`}
                      />
                    </button>
                  </div>
                );
              })}
            </div>
          </section>

          {/* Why is this on */}
          <section className="flex flex-col gap-8 rounded-8 border border-border bg-surface p-16">
            <h2 className="m-0 text-15 font-semibold text-text">Why is this on</h2>
            <p className="m-0 text-13 text-text-secondary">{detail.why_summary}</p>
            <div>
              <Button size="sm" variant="ghost" onClick={() => setShowWhy(true)}>
                Open trace
              </Button>
            </div>
          </section>
        </div>
      </div>

      {showWhy && (
        <WhyTraceDialog
          open={true}
          itemId={detail.item_id}
          agent={detail.agents[0]?.id ?? "claude_code"}
          onClose={() => setShowWhy(false)}
        />
      )}

      {showFixToken && (
        <FixTokenDialog
          secretName={detail.name}
          testConnection={async () => {
            const res = await invoke<McpServerTestView>("mcp_server_test", {
              request: { item_id: detail.item_id, scope: detail.scope, project_id: activeProject },
            });
            return { ok: res.tools_count > 0 };
          }}
          saveServer={async () => {
            await load();
          }}
          onClose={() => setShowFixToken(false)}
        />
      )}
    </main>
  );
}
