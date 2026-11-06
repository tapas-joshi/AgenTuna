import { useState } from "react";
import type { JSX } from "react";
import { Menu, MenuLabel, MenuSeparator } from "./Menu";
import { MenuItem } from "./MenuItem";

/** Stable account-menu destinations. */
export type AccountMenuAction = "preferences" | "backups" | "secrets" | "agents" | "about";

/** Local-only account menu; no identity, sign-in or telemetry service is implied. */
export function AccountMenu({ version, account, onAction }: {
  readonly version: string;
  readonly account?: { readonly name: string; readonly detail?: string };
  readonly onAction?: (action: AccountMenuAction) => void;
}): JSX.Element {
  const [open, setOpen] = useState(false);
  return <Menu open={open} onOpenChange={setOpen} trigger={
    <button type="button" aria-label="Open account menu" aria-haspopup="menu" aria-expanded={open} className="flex w-full items-center gap-8 rounded-6 border border-border bg-raised px-8 py-[6px] text-left focus-visible:outline-2 focus-visible:outline-focus">
      <span aria-hidden="true" className="flex size-[20px] shrink-0 items-center justify-center rounded-full bg-overlay text-12 font-semibold text-text">{(account?.name ?? "AgenTuna").slice(0, 1).toUpperCase()}</span>
      <span className="flex min-w-0 flex-col"><span className="truncate text-12 font-medium text-text">{account?.name ?? "AgenTuna"}</span><span className="truncate text-11 text-text-muted">{account?.detail ?? "No account, no telemetry"}</span></span>
    </button>
  }>
    <MenuLabel>Local app · {version}</MenuLabel>
    <MenuItem label="Preferences" onSelect={() => onAction?.("preferences")} />
    <MenuItem label="Backups & restore" onSelect={() => onAction?.("backups")} />
    <MenuItem label="Keychain secrets" onSelect={() => onAction?.("secrets")} />
    <MenuItem label="Agents on this Mac" onSelect={() => onAction?.("agents")} />
    <MenuSeparator />
    <MenuItem label="About AgenTuna" onSelect={() => onAction?.("about")} />
  </Menu>;
}
