import { useEffect, useRef, useState } from "react";
import type { JSX } from "react";
import { Menu, MenuLabel, MenuSeparator } from "./Menu";
import { MenuItem } from "./MenuItem";

/** Stable account-menu destinations. */
export type AccountMenuAction = "preferences" | "backups" | "secrets" | "agents" | "about";

/** Properties accepted by {@link AccountMenu}. */
export interface AccountMenuProps {
  readonly version: string;
  readonly account?: { readonly name: string; readonly detail?: string };
  readonly onAction?: (action: AccountMenuAction) => void;
  readonly disabledReasons?: Partial<Record<AccountMenuAction, string>>;
  readonly open?: boolean;
  readonly onOpenChange?: (open: boolean) => void;
  readonly routeKey?: string;
  readonly route?: string;
}

/** Local-only account menu; no identity, sign-in or telemetry service is implied. */
export function AccountMenu({
  version,
  account,
  onAction,
  disabledReasons,
  open: controlledOpen,
  onOpenChange,
  routeKey,
  route,
}: AccountMenuProps): JSX.Element {
  const [localOpen, setLocalOpen] = useState(false);
  const open = controlledOpen ?? localOpen;
  const setOpen = onOpenChange ?? setLocalOpen;
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  const activeRoute = route ?? routeKey;
  const previousRoute = useRef(activeRoute);

  useEffect(() => {
    if (previousRoute.current !== undefined && previousRoute.current !== activeRoute) {
      previousRoute.current = activeRoute;
      if (open) {
        setOpen(false);
        setTimeout(() => {
          triggerRef.current?.focus();
        }, 0);
      }
    } else {
      previousRoute.current = activeRoute;
    }
  }, [activeRoute, open, setOpen]);

  return (
    <Menu
      open={open}
      onOpenChange={setOpen}
      trigger={
        <button
          ref={triggerRef}
          type="button"
          aria-label="Open account menu"
          aria-haspopup="menu"
          aria-expanded={open}
          className="flex w-full items-center gap-8 rounded-6 border border-border bg-raised px-8 py-[6px] text-left focus-visible:outline-2 focus-visible:outline-focus"
        >
          <span
            aria-hidden="true"
            className="flex size-[20px] shrink-0 items-center justify-center rounded-full bg-overlay text-12 font-semibold text-text"
          >
            {(account?.name ?? "AgenTuna").slice(0, 1).toUpperCase()}
          </span>
          <span className="flex min-w-0 flex-col">
            <span className="truncate text-12 font-medium text-text">
              {account?.name ?? "AgenTuna"}
            </span>
            <span className="truncate text-11 text-text-muted">
              {account?.detail ?? "No account, no telemetry"}
            </span>
          </span>
        </button>
      }
    >
      <MenuLabel>Local app · {version}</MenuLabel>
      <MenuItem
        label="Preferences"
        disabled={Boolean(disabledReasons?.preferences)}
        disabledReason={disabledReasons?.preferences}
        onSelect={() => onAction?.("preferences")}
      />
      <MenuItem
        label="Backups & restore"
        disabled={Boolean(disabledReasons?.backups)}
        disabledReason={disabledReasons?.backups}
        onSelect={() => onAction?.("backups")}
      />
      <MenuItem
        label="Keychain secrets"
        disabled={Boolean(disabledReasons?.secrets)}
        disabledReason={disabledReasons?.secrets}
        onSelect={() => onAction?.("secrets")}
      />
      <MenuItem
        label="Agents on this Mac"
        disabled={Boolean(disabledReasons?.agents)}
        disabledReason={disabledReasons?.agents}
        onSelect={() => onAction?.("agents")}
      />
      <MenuSeparator />
      <MenuItem
        label="About AgenTuna"
        disabled={Boolean(disabledReasons?.about)}
        disabledReason={disabledReasons?.about}
        onSelect={() => onAction?.("about")}
      />
    </Menu>
  );
}
