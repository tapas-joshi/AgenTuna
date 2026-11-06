/**
 * Inline SVG icons for the shell chrome (TU-006).
 *
 * One 16px line-icon set drawn in `currentColor` so the theme tokens control
 * them. They are decorative: the adjacent label carries the meaning, and every
 * icon is `aria-hidden`.
 */

import type { JSX } from "react";
import type { ShellSectionId } from "./routes";

const ICON_PROPS = {
  viewBox: "0 0 16 16",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
} as const;

/** Grid of four rounded squares — Overview. */
export function OverviewIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <rect x="2" y="2" width="5" height="5" rx="1.2" />
      <rect x="9" y="2" width="5" height="5" rx="1.2" />
      <rect x="2" y="9" width="5" height="5" rx="1.2" />
      <rect x="9" y="9" width="5" height="5" rx="1.2" />
    </svg>
  );
}

/** Four-point sparkle — Skills. */
export function SkillsIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <path d="M8 1.5 9.7 6.3 14.5 8 9.7 9.7 8 14.5 6.3 9.7 1.5 8 6.3 6.3Z" />
      <path d="M12.5 11.5 13.3 13.7 15.5 14.5 13.3 15.3 12.5 17.5 11.7 15.3 9.5 14.5 11.7 13.7Z" />
    </svg>
  );
}

/** Server rack — MCP Servers. */
export function McpIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <rect x="2" y="2" width="12" height="5" rx="1" />
      <rect x="2" y="9" width="12" height="5" rx="1" />
      <path d="M4.5 4.5h.01M4.5 11.5h.01" strokeWidth="2" />
      <path d="M7 4.5h5M7 11.5h5" />
    </svg>
  );
}

/** Document with folded corner — Instructions. */
export function InstructionsIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <path d="M4 1.8h5.2L12 4.6v9.6H4Z" />
      <path d="M9.2 1.8V4.8H12" />
      <path d="M6 7.5h4M6 10h4" />
    </svg>
  );
}

/** Briefcase — Agents. */
export function AgentsIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <rect x="1.8" y="5" width="12.4" height="8.5" rx="1.2" />
      <path d="M5.5 5V3.8a1.2 1.2 0 0 1 1.2-1.2h2.6a1.2 1.2 0 0 1 1.2 1.2V5" />
      <path d="M1.8 8.8h12.4" />
    </svg>
  );
}

/** Stacked layers — Effective config. */
export function EffectiveIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <path d="m8 2 5.5 3L8 8 2.5 5Z" />
      <path d="m2.5 8 5.5 3 5.5-3" />
      <path d="m2.5 11 5.5 3 5.5-3" />
    </svg>
  );
}

/** Circular arrows — Sync & drift. */
export function DriftIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[14px]">
      <path d="M13.2 8a5.2 5.2 0 0 1-9.3 3.1" />
      <path d="M2.8 8a5.2 5.2 0 0 1 9.3-3.1" />
      <path d="M11.6 2.2v2.9H8.7M4.4 13.8V10.9h2.9" />
    </svg>
  );
}

/** Magnifier — palette/search hint in the top bar. */
export function SearchIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[12px]">
      <circle cx="7" cy="7" r="4.4" />
      <path d="m10.4 10.4 3.1 3.1" />
    </svg>
  );
}

/** Chevron pointing down — picker triggers. */
export function ChevronDownIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[12px]">
      <path d="m4 6 4 4 4-4" />
    </svg>
  );
}

/** Folder — project picker trigger. */
export function ProjectIcon(): JSX.Element {
  return (
    <svg {...ICON_PROPS} className="size-[12px]">
      <path d="M1.8 4.2a1 1 0 0 1 1-1h3l1.4 1.6h6.8a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H2.8a1 1 0 0 1-1-1Z" />
    </svg>
  );
}

/** Avatar fallback: initials rendered as text, not an icon. */

/** Maps a sidebar section to its icon component. */
export const SECTION_ICONS: Readonly<Record<ShellSectionId, () => JSX.Element>> = {
  overview: OverviewIcon,
  skills: SkillsIcon,
  mcp: McpIcon,
  instructions: InstructionsIcon,
  agents: AgentsIcon,
  effective: EffectiveIcon,
  drift: DriftIcon,
};
