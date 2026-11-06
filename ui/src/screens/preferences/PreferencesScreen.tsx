import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState, type ReactNode } from "react";
import type { NetworkPolicyView } from "../../generated/NetworkPolicyView";
import type { AppPathsView } from "../../generated/AppPathsView";
import { Button } from "../../components/button";
import { StateView } from "../../components/states";
import { LogViewer } from "../logs/LogViewer";

interface SettingsDocument { readonly schema_version: number; readonly values: Record<string, unknown> }
type Preferences = {
  health_interval_minutes: number;
  background_health: "off" | "local" | "all";
  backup_retention_days: number;
  default_link_mode: "link" | "copy";
  link_modes: Record<string, "link" | "copy">;
  native_banner_hidden: Record<string, boolean>;
  marketplace_enabled: boolean;
  auto_update_skills: boolean;
};
const DEFAULTS: Preferences = { health_interval_minutes: 5, background_health: "local", backup_retention_days: 30, default_link_mode: "link", link_modes: {}, native_banner_hidden: {}, marketplace_enabled: false, auto_update_skills: false };
const ITEM_KINDS = ["skill", "mcp_server", "rule"] as const;

function decodeDocument(document: SettingsDocument): Preferences {
  const linkModes: Preferences["link_modes"] = {};
  const banners: Preferences["native_banner_hidden"] = {};
  for (const [key, item] of Object.entries(document.values)) {
    if (key.startsWith("link_mode.")) linkModes[key.slice("link_mode.".length)] = item === "copy" ? "copy" : "link";
    if (key.startsWith("native_banner_hidden.")) banners[key.slice("native_banner_hidden.".length)] = item === true;
  }
  const values = document.values;
  return {
    health_interval_minutes: typeof values.health_interval_minutes === "number" && [0, 5, 15, 60].includes(values.health_interval_minutes) ? values.health_interval_minutes : DEFAULTS.health_interval_minutes,
    background_health: values.background_health === "off" || values.background_health === "all" ? values.background_health : DEFAULTS.background_health,
    backup_retention_days: typeof values.backup_retention_days === "number" && values.backup_retention_days >= 30 ? values.backup_retention_days : DEFAULTS.backup_retention_days,
    default_link_mode: values.default_link_mode === "copy" ? "copy" : DEFAULTS.default_link_mode,
    link_modes: linkModes, native_banner_hidden: banners,
    marketplace_enabled: values["network.marketplace_enabled"] === true,
    auto_update_skills: values.auto_update_skills === true,
  };
}

/** Preferences view. Values and validation are owned by the typed backend settings commands. */
export function PreferencesScreen() {
  const [value, setValue] = useState<Preferences | null>(null);
  const [policy, setPolicy] = useState<NetworkPolicyView | null>(null);
  const [paths, setPaths] = useState<AppPathsView | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [fieldError, setFieldError] = useState<string | null>(null);
  const [saving, setSaving] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [agentId, setAgentId] = useState("");

  const reload = () => {
    setFailure(null);
    void Promise.all([invoke<SettingsDocument>("settings_load"), invoke<NetworkPolicyView>("network_policy"), invoke<AppPathsView>("app_paths")]).then(([document, nextPolicy, nextPaths]) => {
      setValue(decodeDocument(document)); setPolicy(nextPolicy); setPaths(nextPaths);
    }, (error: unknown) => setFailure(String(error)));
  };
  useEffect(reload, []);

  const update = async (key: string, next: unknown) => {
    if (value === null) return;
    if (key === "backup_retention_days" && (typeof next !== "number" || next < 30)) { setFieldError("Retention must be at least 30 days. No change was saved."); return; }
    if (key === "health_interval_minutes" && ![0, 5, 15, 60].includes(Number(next))) { setFieldError("Choose off, 5, 15 or 60 minutes. No change was saved."); return; }
    setFieldError(null);
    const previous = value;
    setSaving(key); setNotice(null); setFailure(null);
    try {
      await invoke("settings_set", { key, value: next });
      setValue(decodeDocument(await invoke<SettingsDocument>("settings_load"))); setNotice("Preferences saved");
    } catch (error) { setValue(previous); setFailure(String(error)); }
    finally { setSaving(null); }
  };
  const reset = async () => {
    const defaults: readonly [string, unknown][] = [["health_interval_minutes", 5], ["background_health", "local"], ["backup_retention_days", 30], ["default_link_mode", "link"], ["network.marketplace_enabled", false], ["auto_update_skills", false]];
    setSaving("reset"); setNotice(null); setFailure(null);
    try {
      for (const [key, next] of defaults) await invoke("settings_set", { key, value: next });
      for (const kind of ITEM_KINDS) await invoke("settings_set", { key: `native_banner_hidden.${kind}`, value: false });
      for (const agent of Object.keys(value?.link_modes ?? {})) await invoke("settings_set", { key: `link_mode.${agent}`, value: "link" });
      await reload(); setNotice("Preferences reset to defaults");
    } catch (error) { setFailure(String(error)); }
    finally { setSaving(null); }
  };

  if (failure !== null && value === null) return <StateView variant="error" title="Couldn't load preferences" body={failure} actions={[{ label: "Retry", onClick: reload }]} />;
  if (value === null) return <StateView variant="loading" title="Loading preferences" />;
  const row = (label: string, content: ReactNode, detail?: string) => <div className="grid grid-cols-[minmax(180px,1fr)_minmax(180px,280px)] items-center gap-16 border-b border-border py-12"><div><div className="text-13 font-medium text-text">{label}</div>{detail && <div className="mt-4 text-12 text-muted">{detail}</div>}</div><div>{content}</div></div>;
  const select = (label: string, options: readonly { value: string; label: string }[], current: string, key: string, disabled = false) => <select aria-label={label} className="w-full rounded-6 border border-border bg-raised px-10 py-8 text-text" value={current} disabled={disabled || saving === key} onChange={event => { const option = options.find(item => item.value === event.target.value); if (option) void update(key, key === "health_interval_minutes" ? Number(option.value) : option.value); }}>{options.map(item => <option key={item.value} value={item.value}>{item.label}</option>)}</select>;
  const modeOptions = [{ value: "link", label: "Link" }, { value: "copy", label: "Copy" }];
  return <div className="mx-auto max-w-[920px] space-y-16 p-24" data-testid="preferences-screen">
    <header><h1 className="m-0 text-22 font-semibold text-text">Preferences</h1><p className="mt-6 text-13 text-secondary">Network checks require an explicit request or enabled automatic updates.</p></header>
    {failure && <div role="alert" className="rounded-6 border border-danger/40 bg-danger/10 p-12 text-13 text-danger">{failure} <Button variant="secondary" onClick={reload}>Retry</Button></div>}
    {fieldError && <p role="alert" className="m-0 text-13 text-danger">{fieldError}</p>}{notice && <div role="status" className="text-13 text-success">{notice}</div>}
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">General</h2>
      {row("Default link mode", select("Default link mode", modeOptions, value.default_link_mode, "default_link_mode"))}
      {row("Per-agent link mode", <div className="flex gap-8"><input aria-label="Agent id" value={agentId} onChange={event => setAgentId(event.target.value)} placeholder="agent id" className="min-w-0 rounded-6 border border-border bg-raised px-8 py-6 text-text" />{select("Link mode for agent", modeOptions, value.link_modes[agentId] ?? value.default_link_mode, `link_mode.${agentId}`, agentId.trim() === "")}</div>, "Agent-specific mode overrides the default.")}
      {row("Native item banners", <div className="flex flex-wrap gap-10">{ITEM_KINDS.map(kind => <label key={kind} className="flex items-center gap-6 text-12 text-text"><input type="checkbox" checked={value.native_banner_hidden[kind] ?? false} onChange={event => void update(`native_banner_hidden.${kind}`, event.target.checked)} />Hide {kind === "skill" ? "Skills" : kind === "mcp_server" ? "MCP servers" : "Rules"} banner</label>)}</div>, "Uncheck a kind to show its banner again.")}
    </section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Health</h2>{row("Health interval", select("Health interval", [{ value: "0", label: "Off" }, { value: "5", label: "5 minutes" }, { value: "15", label: "15 minutes" }, { value: "60", label: "60 minutes" }], String(value.health_interval_minutes), "health_interval_minutes"))}{row("Background health", select("Background health", [{ value: "off", label: "Off" }, { value: "local", label: "Local" }, { value: "all", label: "All" }], value.background_health, "background_health"), "Local means tested stdio servers; HTTP requires All.")}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Skill updates</h2>{row("Automatically check and update skills", <button type="button" role="switch" aria-checked={value.auto_update_skills} aria-label="Automatically update skills" disabled={saving === "auto_update_skills"} className="rounded-6 border border-border px-12 py-8 text-13 text-text" onClick={() => void update("auto_update_skills", !value.auto_update_skills)}>{value.auto_update_skills ? "On" : "Off"}</button>, "Off by default. Only skills configured to follow automatic updates are eligible.")}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Backups</h2>{row("Retention (days)", <input aria-label="Backup retention days" type="number" min={30} value={value.backup_retention_days} onChange={event => void update("backup_retention_days", Number(event.target.value))} className="w-full rounded-6 border border-border bg-raised px-10 py-8 text-text" />, "Minimum 30 days.")}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Network</h2>{row("Marketplace access", <button type="button" role="switch" aria-checked={value.marketplace_enabled} aria-label="Marketplace access" disabled={saving === "network.marketplace_enabled"} className="rounded-6 border border-border px-12 py-8 text-13 text-text" onClick={() => void update("network.marketplace_enabled", !value.marketplace_enabled)}>{value.marketplace_enabled ? "On" : "Off"}</button>)}{row("Network policy", <span className="text-13 text-secondary">{policy?.statement ?? "Policy unavailable"} {policy?.offline && <strong className="text-warning">Offline</strong>}</span>)}{policy && <div className="mt-12 space-y-8">{policy.actions.map(action => <div key={action.action} className="flex items-center justify-between gap-12 rounded-6 bg-raised px-12 py-8 text-13"><span>{action.label}</span><span className={action.state === "blocked" ? "text-warning" : "text-secondary"}>{action.state.replaceAll("_", " ")}{action.reason ? ` — ${action.reason}` : ""}</span></div>)}</div>}{policy?.events.length ? <section aria-label="Recent network events" className="mt-16"><h3 className="text-13 font-medium text-text">Recent network activity</h3>{policy.events.map((event, index) => <div key={`${event.at}-${event.action}-${index}`} className="flex justify-between gap-8 border-t border-border py-6 text-11 text-secondary"><span>{event.action} · {event.host ?? "host unavailable"}</span><span>{event.at} · {event.bytes} bytes</span></div>)}</section> : null}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Storage</h2>{row("Global store path", <code className="break-all text-12 text-secondary">{paths?.global_store ?? "Unavailable"}</code>)}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Reset</h2>{row("Reset preferences", <Button variant="destructive" loading={saving === "reset"} onClick={() => void reset()}>Reset settings</Button>)}</section>
    <section className="rounded-10 border border-border bg-surface p-16"><h2 className="m-0 text-15 font-semibold text-text">Window</h2>{row("Window layout", <Button variant="secondary" onClick={() => void invoke("window_state_reset").then(() => setNotice("Window layout reset"), error => setFailure(String(error)))}>Reset Window Layout</Button>, "Restore the default size and position.")}</section>
    <details className="rounded-10 border border-border bg-surface p-16"><summary className="cursor-pointer text-15 font-semibold text-text">Logs & diagnostics</summary><LogViewer /></details>
  </div>;
}
