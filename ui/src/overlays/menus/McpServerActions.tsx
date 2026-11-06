/**
 * MCP row actions (TM-025).
 *
 * The menu is presentation only: it reports the chosen action and the reason a row
 * is unavailable, and the screen owns the dialogs, previews, writes and toasts.
 * Native-only rows route `Open` to adoption, and every destructive item explains
 * itself before it can be chosen.
 */

import { Menu, MenuSeparator } from "./Menu";
import { MenuItem } from "./MenuItem";

/** Every action the MCP row menu offers. */
export type McpServerAction =
  | "open"
  | "edit"
  | "why"
  | "test"
  | "disable-repo"
  | "disable-agent"
  | "delete"
  | "lock"
  | "unlock";

/** Properties of {@link McpServerActions}. */
export interface McpServerActionsProps {
  /** Server display name; also the trigger's accessible name. */
  readonly name: string;
  /** Whether the row exists only in an agent's own folder. */
  readonly native: boolean;
  /** Whether the row currently holds a lock. */
  readonly locked: boolean;
  /** Whether the active scope is Global, the only scope where locks apply. */
  readonly canLock: boolean;
  /** Why Global writes are unavailable, when they are; disables Lock. */
  readonly readOnlyReason?: string | null;
  /** Whether the menu is open. */
  readonly open: boolean;
  /** Reports open changes (Radix closes the menu on selection and Escape). */
  readonly onOpenChange: (open: boolean) => void;
  /** Receives the chosen action. */
  readonly onSelect: (action: McpServerAction) => void;
}

/** Row overflow menu for one MCP server. */
export function McpServerActions({
  name,
  native,
  locked,
  canLock,
  readOnlyReason = null,
  open,
  onOpenChange,
  onSelect,
}: McpServerActionsProps) {
  const select = (action: McpServerAction) => () => onSelect(action);
  const notInStudio = native ? "Adopt this server first" : undefined;
  return (
    <Menu
      open={open}
      onOpenChange={onOpenChange}
      align="end"
      trigger={
        <button
          type="button"
          aria-label={`Actions for ${name}`}
          aria-haspopup="menu"
          className="rounded-6 border border-border bg-surface px-8 py-4 text-13"
        >
          ⋯
        </button>
      }
    >
      <MenuItem
        label={native ? "Adopt…" : "Open"}
        mnemonic="o"
        onSelect={select("open")}
      />
      <MenuItem
        label="Edit"
        mnemonic="e"
        disabled={native}
        disabledReason={notInStudio}
        onSelect={select("edit")}
      />
      <MenuItem label="Why is this on?" mnemonic="w" onSelect={select("why")} />
      <MenuItem label="Test connection" mnemonic="t" onSelect={select("test")} />
      <MenuSeparator />
      <MenuItem
        label="Disable in this repo"
        mnemonic="d"
        disabled={native}
        disabledReason={notInStudio}
        onSelect={select("disable-repo")}
      />
      <MenuItem
        label="Disable for this agent"
        mnemonic="a"
        disabled={native}
        disabledReason={notInStudio}
        onSelect={select("disable-agent")}
      />
      <MenuSeparator />
      {canLock ? (
        locked ? (
          <MenuItem label="Unlock…" mnemonic="u" onSelect={select("unlock")} />
        ) : (
          <MenuItem
            label="Lock…"
            mnemonic="l"
            disabled={readOnlyReason !== null}
            disabledReason={readOnlyReason ?? undefined}
            onSelect={select("lock")}
          />
        )
      ) : null}
      <MenuItem
        label="Delete server"
        mnemonic="x"
        tone="danger"
        disabled={native}
        disabledReason={native ? "Native servers are adopted, never deleted" : undefined}
        onSelect={select("delete")}
      />
    </Menu>
  );
}
