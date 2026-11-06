import { useState } from "react";
import type { NetworkActionView } from "../../generated/NetworkActionView";
import type { NetworkPolicyView } from "../../generated/NetworkPolicyView";

export interface NetworkActionGateProps {
  readonly policy: NetworkPolicyView;
  readonly action: string;
  readonly host?: string;
  readonly onRun: (action: string, host: string | undefined) => void;
  readonly onConfirm: (action: string, host: string) => void;
  readonly onCancel: () => void;
}

/** Renders a decision only from the central network policy; never starts networking itself. */
export function NetworkActionGate({ policy, action, host, onRun, onConfirm, onCancel }: NetworkActionGateProps) {
  const [confirming, setConfirming] = useState(false);
  const item = policy.actions.find(candidate => candidate.action === action);
  if (!item) return null;
  const blocked = policy.offline || item.state === "blocked";
  const reason = policy.offline ? item.reason ?? "Offline mode disables network actions." : item.reason;
  const execute = () => {
    if (blocked) return;
    if (item.state === "confirmation_required") {
      if (!confirming) { setConfirming(true); return; }
      if (!host) return;
      onConfirm(item.action, host);
      setConfirming(false);
      return;
    }
    onRun(item.action, host);
  };
  return <div className="flex flex-wrap items-center gap-10" data-network-action={item.action}>
    <span className="text-13 text-secondary">{item.label}</span>
    {policy.offline && <span className="rounded-3 bg-warning/10 px-6 py-2 text-11 font-medium text-warning">Offline</span>}
    {blocked ? <span role="status" className="text-12 text-warning">{reason ?? "Blocked by network policy"}</span> :
      <button type="button" className="rounded-6 border border-border px-10 py-6 text-12 text-text hover:bg-raised focus-visible:outline-2 focus-visible:outline-focus" onClick={execute}>{item.state === "confirmation_required" ? "Continue" : "Run"}</button>}
    {confirming && !blocked && <div role="alertdialog" aria-label={`Confirm ${item.label}`} className="basis-full rounded-6 border border-warning/40 bg-warning/10 p-12 text-13 text-text">
      <p className="m-0">Connect to <strong>{host ?? "unknown destination"}</strong>?</p>
      <p className="mt-4 text-12 text-secondary">This network action is not run until you confirm.</p>
      <div className="mt-10 flex gap-8"><button type="button" disabled={!host} className="rounded-6 bg-accent px-10 py-6 text-12 text-white disabled:opacity-50" onClick={() => execute()}>Confirm connection</button><button type="button" className="rounded-6 border border-border px-10 py-6 text-12 text-text" onClick={() => { setConfirming(false); onCancel(); }}>Cancel</button></div>
    </div>}
  </div>;
}

/** Lists the exact action set provided by TF-036, with policy reasons and offline labels. */
export function NetworkActionRegistry({ policy, onRun, onConfirm, onCancel }: {
  readonly policy: NetworkPolicyView;
  readonly onRun: NetworkActionGateProps["onRun"];
  readonly onConfirm: NetworkActionGateProps["onConfirm"];
  readonly onCancel: NetworkActionGateProps["onCancel"];
}) {
  return <div className="space-y-10" aria-label="Network actions">{policy.actions.map((item: NetworkActionView) => <NetworkActionGate key={item.action} policy={policy} action={item.action} onRun={onRun} onConfirm={onConfirm} onCancel={onCancel} />)}</div>;
}
