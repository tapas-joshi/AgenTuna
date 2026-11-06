import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { RemoteBackupSettingsView } from "../../generated/RemoteBackupSettingsView";
import { Button } from "../../components/button";

export function RemoteBackupSection() {
  const [settings, setSettings] = useState<RemoteBackupSettingsView | null>(null);
  const [deviceName, setDeviceName] = useState("");
  const [repoUrl, setRepoUrl] = useState("");
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const load = () => void invoke<RemoteBackupSettingsView>("remote_backup_settings").then(value => {
    setSettings(value); setDeviceName(value.device_name); setRepoUrl(value.repo_url ?? ""); setError(null);
  }, reason => setError(String(reason)));
  useEffect(load, []);
  const save = async () => {
    setError(null); setBusy(true);
    try {
      const result = await invoke<RemoteBackupSettingsView>("remote_backup_configure", {
        deviceName, repoUrl, token: token.length === 0 ? null : token,
      });
      setSettings(result); setDeviceName(result.device_name); setRepoUrl(result.repo_url ?? ""); setToken("");
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  };
  return <section className="rounded-10 border border-border bg-surface p-16" aria-label="Remote backup">
    <h2 className="m-0 text-15 font-semibold text-text">Remote backup</h2>
    <p className="mt-6 text-12 text-muted">Credentials are stored in the system credential store. The token is never shown after saving.</p>
    {error && <div role="alert" className="my-8 text-13 text-danger">{error} <Button variant="secondary" onClick={load}>Retry</Button></div>}
    {settings && <div className="mt-12 grid gap-12">
      <label className="grid gap-4 text-13 text-text">Device name<input aria-label="Remote backup device name" value={deviceName} onChange={event => setDeviceName(event.target.value)} className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      <label className="grid gap-4 text-13 text-text">GitHub repository URL<input aria-label="Remote backup repository URL" value={repoUrl} onChange={event => setRepoUrl(event.target.value)} placeholder="https://github.com/owner/repository.git" className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      <label className="grid gap-4 text-13 text-text">GitHub token{settings.token_saved && <span className="text-12 text-success">Connected token saved</span>}<input aria-label="GitHub token" type="password" autoComplete="new-password" value={token} onChange={event => setToken(event.target.value)} placeholder={settings.token_saved ? "Token saved; enter to replace" : "Paste a GitHub token"} className="rounded-6 border border-border bg-raised px-10 py-8" /></label>
      <div className="flex gap-8"><Button loading={busy} onClick={() => void save()}>Save remote backup settings</Button><Button variant="secondary" onClick={load}>Refresh status</Button></div>
      <div role="status" className="grid gap-4 text-12 text-secondary"><span>Status: {settings.status}{settings.error_kind ? ` (${settings.error_kind})` : ""}</span><span>Last backup: {settings.last_push_at ?? "Never"}</span><span>Last commit: {settings.last_commit ?? "None"}</span></div>
    </div>}
  </section>;
}
