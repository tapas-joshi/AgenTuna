/**
 * App shell barrel (TU-006).
 *
 * Chrome only: sidebar, top bar and the routed frame. Routing behaviour is
 * TU-010's, scope state TA-053's, the scope widget TU-040's.
 */

export { Shell } from "./Shell";
export type { ShellProps } from "./Shell";
export { Sidebar } from "./Sidebar";
export type { SidebarProps } from "./Sidebar";
export { TopBar } from "./TopBar";
export type { TopBarProps } from "./TopBar";
export {
  SHELL_SECTION_LABEL,
  SHELL_SECTION_ROUTE,
  SHELL_SECTIONS,
  sectionForRoute,
} from "./routes";
export type {
  ShellAccount,
  ShellAgentStatus,
  ShellSectionId,
} from "./routes";
