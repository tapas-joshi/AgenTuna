/**
 * Data shapes rendered by the table's per-agent cells.
 */

/** One detected agent, rendered as one column of the table. */
export interface AgentDescriptor {
  /** Stable id used by the toggle callback and row keys. */
  readonly id: string;
  /** Column header and accessible name, for example `Claude Code`. */
  readonly label: string;
}

/**
 * The six agent states a cell can show.
 *
 * `linked` and `off` render a switch; `native` renders a badge plus its source
 * path; `none` renders a dash; `locked` renders a disabled switch with a lock
 * glyph; `shadowed` renders a status dot naming the winning scope.
 */
export type AgentCellState =
  | {
      /** Deployed by AgenTuna and unchanged since the last write. */
      readonly kind: "linked";
      /** The agent file is read-only; the switch renders disabled. */
      readonly readOnly?: boolean;
      /** A write is in flight; the switch renders disabled and busy. */
      readonly pending?: boolean;
      /** Why the switch is disabled; shown as the tooltip instead of the status text. */
      readonly disabledReason?: string;
    }
  | {
      /** Not deployed to this agent. */
      readonly kind: "off";
      /** The agent file is read-only; the switch renders disabled. */
      readonly readOnly?: boolean;
      /** A write is in flight; the switch renders disabled and busy. */
      readonly pending?: boolean;
      /** Why the switch is disabled; shown as the tooltip instead of the status text. */
      readonly disabledReason?: string;
    }
  | {
      /** Found only in the agent's own folder, outside AgenTuna management. */
      readonly kind: "native";
      /** Absolute or `~/`-relative source path, shown as text and as the tooltip. */
      readonly path: string;
      /** The agent folder is read-only until the item is adopted. */
      readonly readOnly?: boolean;
      /**
       * Agents that load the same folder. Codex and omp share
       * `<project>/.agents/skills`, so their cell is one cell, not two switches.
       */
      readonly sharedWith?: readonly string[];
    }
  | {
      /** The agent was detected but the item is not present. */
      readonly kind: "none";
    }
  | {
      /** A Global lock prevents changes from lower scopes. */
      readonly kind: "locked";
      /** Scope or setting that owns the lock, shown in the tooltip. */
      readonly lockedBy: string;
      /** Whether the locked entry is currently deployed. Defaults to `true`. */
      readonly enabled?: boolean;
    }
  | {
      /** A higher-precedence scope overrides this agent's entry. */
      readonly kind: "shadowed";
      /** Scope label that wins, shown in the tooltip. */
      readonly shadowedBy: string;
    };
