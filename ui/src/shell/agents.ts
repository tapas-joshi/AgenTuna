/**
 * Maps the typed agent detection result onto the sidebar's status rows.
 *
 * The sidebar is presentation: it renders exactly what `agents_list` or
 * `agents_rescan` returned, so "detected" and "not found" are the backend's
 * presence values rather than a second guess in the webview.
 */
import type { AgentCardView } from "../generated/AgentCardView";
import type { ShellAgentStatus } from "./routes";

/**
 * Converts detected agent cards into sidebar rows.
 *
 * `configured_only` counts as detected: the agent's configuration was found on
 * this Mac even when its CLI is missing. Only `not_installed` renders as
 * "not found". The backend's order is preserved.
 */
export function shellAgentStatus(
  cards: readonly AgentCardView[],
): readonly ShellAgentStatus[] {
  return cards.map((card) => ({
    id: card.id,
    label: card.name === "" ? card.id : card.name,
    detected: card.presence !== "not_installed",
  }));
}
