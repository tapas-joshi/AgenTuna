import type { JSX } from "react";
import { Button } from "../button";

export interface NativeBannerProps {
  readonly kind: "skill" | "mcp_server";
  readonly count: number;
  readonly onReviewMigrate: () => void;
  readonly onHide: () => void;
}

/**
 * Banner rendered above skills or MCP server tables when unmanaged items exist in agent folders (TC-025).
 */
export function NativeBanner({
  kind,
  count,
  onReviewMigrate,
  onHide,
}: NativeBannerProps): JSX.Element | null {
  if (count <= 0) return null;

  const itemNoun =
    kind === "skill"
      ? count === 1
        ? "skill"
        : "skills"
      : count === 1
        ? "MCP server"
        : "MCP servers";

  return (
    <aside
      role="region"
      aria-label="Native items outside AgenTuna"
      className="flex items-center justify-between gap-16 rounded-10 border border-info/40 bg-info/10 px-16 py-12"
      data-slot="native-banner"
    >
      <div className="flex items-center gap-10">
        <span aria-hidden="true" className="text-16 text-info">
          ℹ
        </span>
        <p className="m-0 text-13 font-medium text-text">
          {count} {itemNoun} in agent folders {count === 1 ? "is" : "are"} not in AgenTuna
        </p>
      </div>
      <div className="flex items-center gap-8">
        <Button size="sm" variant="secondary" onClick={onReviewMigrate}>
          Review & migrate
        </Button>
        <Button size="sm" variant="ghost" onClick={onHide}>
          Hide
        </Button>
      </div>
    </aside>
  );
}
