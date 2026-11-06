/** Searchable, stable inventory used by the command palette (TU-014). */
export type PaletteKind = "action" | "skill" | "server" | "rule" | "agent";

/** One indexed entity; `run` is the same typed intent dispatcher used elsewhere. */
export interface PaletteEntry {
  readonly id: string;
  readonly kind: PaletteKind;
  readonly title: string;
  readonly subtitle: string;
  readonly keywords: readonly string[];
  readonly run?: () => void;
}

/** Inputs used to rebuild the index from current snapshots. */
export interface PaletteInventory {
  readonly skills: readonly Omit<PaletteEntry, "kind">[];
  readonly servers: readonly Omit<PaletteEntry, "kind">[];
  readonly rules: readonly Omit<PaletteEntry, "kind">[];
  readonly agents: readonly Omit<PaletteEntry, "kind">[];
  readonly actions: readonly Omit<PaletteEntry, "kind">[];
}

/** Build the complete inventory. Stable IDs are provided by their source snapshots/actions. */
export function buildPaletteIndex(inventory: PaletteInventory): readonly PaletteEntry[] {
  return [
    ...inventory.skills.map((entry) => ({ ...entry, kind: "skill" as const })),
    ...inventory.servers.map((entry) => ({ ...entry, kind: "server" as const })),
    ...inventory.rules.map((entry) => ({ ...entry, kind: "rule" as const })),
    ...inventory.agents.map((entry) => ({ ...entry, kind: "agent" as const })),
    ...inventory.actions.map((entry) => ({ ...entry, kind: "action" as const })),
  ];
}

function matchScore(entry: PaletteEntry, query: string): number | null {
  const q = query.toLocaleLowerCase();
  const fields = [entry.title, entry.subtitle, ...entry.keywords].map((value) => value.toLocaleLowerCase());
  let best: number | null = null;
  for (const field of fields) {
    let pos = 0;
    let score = 0;
    let previous = -2;
    let matched = true;
    for (const char of q) {
      pos = field.indexOf(char, pos);
      if (pos < 0) { matched = false; break; }
      const boundary = pos === 0 || /[^\p{L}\p{N}]/u.test(field[pos - 1] ?? "");
      score += boundary ? 12 : 0;
      score += pos === previous + 1 ? 8 : 0;
      score -= pos * 0.01;
      previous = pos++;
    }
    if (matched) best = Math.max(best ?? -Infinity, score);
  }
  return best;
}

/** Case-insensitive subsequence search with prefix, word-start and run bonuses. */
export function searchPalette(index: readonly PaletteEntry[], query: string, limitPerKind = 50): readonly PaletteEntry[] {
  const normalized = query.trim();
  if (!normalized) return [];
  const ranked: { entry: PaletteEntry; score: number }[] = [];
  for (const entry of index) {
    const score = matchScore(entry, normalized);
    if (score === null) continue;
    const first = entry.title.toLocaleLowerCase();
    const q = normalized.toLocaleLowerCase();
    ranked.push({ entry, score: score + (first.startsWith(q) ? 100 : 0) });
  }
  ranked.sort((a, b) => b.score - a.score || a.entry.title.localeCompare(b.entry.title) || a.entry.id.localeCompare(b.entry.id));
  const counts = new Map<PaletteKind, number>();
  return ranked.flatMap(({ entry }) => {
    const count = counts.get(entry.kind) ?? 0;
    if (count >= Math.max(0, limitPerKind)) return [];
    counts.set(entry.kind, count + 1);
    return [entry];
  });
}

/** Return recent entries plus high-priority actions for the idle palette. */
export function idlePaletteEntries(index: readonly PaletteEntry[], recentIds: readonly string[]): readonly PaletteEntry[] {
  const byId = new Map(index.map((entry) => [entry.id, entry]));
  const recent = recentIds.slice(0, 8).flatMap((id) => { const entry = byId.get(id); return entry ? [entry] : []; });
  const actions = index.filter((entry) => entry.kind === "action").slice(0, 6);
  return [...recent, ...actions.filter((action) => !recent.some((entry) => entry.id === action.id))];
}
