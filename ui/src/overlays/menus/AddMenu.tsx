import { useEffect, useRef, useState } from "react";
import type { JSX, ReactElement } from "react";
import { Menu } from "./Menu";
import { MenuItem } from "./MenuItem";
import type { ScopeToken } from "../../theme";

/** Stable action IDs for the Add menu and other action surfaces. */
export const ADD_ACTION_IDS = ["add.skill", "add.server", "add.rule"] as const;
export type AddActionId = (typeof ADD_ACTION_IDS)[number];

/** One action issued through the application's shared typed action dispatcher. */
export interface AddMenuAction {
  readonly id: AddActionId;
  readonly scope: ScopeToken;
}

/** Props for the top-bar Add menu. */
export interface AddMenuProps {
  readonly scope: ScopeToken;
  readonly disabledReason?: string;
  readonly onAction: (action: AddMenuAction) => void;
  readonly trigger?: ReactElement;
  readonly open?: boolean;
  readonly onOpenChange?: (open: boolean) => void;
  readonly routeKey?: string;
}

/** Add flow menu; keyboard behavior and focus restoration come from Radix Menu. */
export function AddMenu({ scope, disabledReason, onAction, trigger, open: controlledOpen, onOpenChange, routeKey }: AddMenuProps): JSX.Element {
  const [localOpen, setLocalOpen] = useState(false);
  const open = controlledOpen ?? localOpen;
  const setOpen = onOpenChange ?? setLocalOpen;
  const previousRoute = useRef(routeKey);
  useEffect(() => {
    if (previousRoute.current !== routeKey) {
      previousRoute.current = routeKey;
      setOpen(false);
    }
  }, [routeKey, setOpen]);
  const triggerButton = trigger ?? <button type="button" aria-label="Add" className="inline-flex h-[28px] items-center gap-6 rounded-8 bg-accent px-12 text-on-accent"><span aria-hidden="true">+</span>Add</button>;
  const select = (id: AddActionId) => { onAction({ id, scope }); setOpen(false); };
  return <Menu open={open} onOpenChange={setOpen} trigger={triggerButton} align="end">
    <MenuItem label="New skill" shortcut="⌘⇧S" disabled={disabledReason !== undefined} disabledReason={disabledReason} onSelect={() => select("add.skill")} />
    <MenuItem label="Add MCP server" shortcut="⌘⇧M" disabled={disabledReason !== undefined} disabledReason={disabledReason} onSelect={() => select("add.server")} />
    <MenuItem label="New rule file" disabled={disabledReason !== undefined} disabledReason={disabledReason} onSelect={() => select("add.rule")} />
  </Menu>;
}
