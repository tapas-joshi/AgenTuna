# Product name research

Preliminary screening of candidate names for the local-first agent config
manager then described as **Agentic AI Config Studio** (now **AgenTuna**).

Research date: **2026-11-04**. This is naming research, not trademark or legal
advice. Nothing in this report establishes that a name is available, registrable
or safe to launch. No rename, package install or file outside this report was
changed.

## Bottom line

No candidate in this set is both distinctive and clearly unclaimed. The five
supplied names split into three hard collisions and two thin slates:

- **Reject: Agentrove, Agentry, Scopewright.** Each has an existing product in
  the same category, and Scopewright's npm package implements a managed
  multi-harness installer with ownership state — the closest functional overlap
  found in this research.
- **No product match found: Configrove, Scopary.** Both returned no indexed
  software product, but each carries a different weakness: Configrove borrows
  the `grove` morpheme from an exact-category competitor and a generic `config`
  prefix; Scopary's `.com` is registered and parked.
- **Invented survivor: Configfold.** No npm package, no GitHub repository and no
  indexed product were found. It is clear but awkward: it reads as
  "config folder".

Proposed shortlist, in order: **Configrove**, **Scopary**, **Configfold**.
These are candidates for a formal clearance search, not cleared names.

## Method and limits

Searches covered:

- npm registry document lookup and npm search index for each exact spelling;
- GitHub repository, user and organisation search;
- WHOIS for `.com` candidates and DNS resolution for `.com`, `.dev`, `.app`,
  `.io` variants;
- general web search for the exact spelling plus "software", "app" or "AI";
- first-party pages and READMEs for every conflict treated as material.

Not searched: trademark registers (USPTO, EUIPO, UKIPO), company registries,
app stores, social handles, package registries other than npm, or paid clearance
databases. A missing search hit is **not** evidence of availability: unindexed
products, private repositories, common-law marks and pending applications do not
appear in these sources. WHOIS records also vary by registry and can change at
any time.

## The space is already crowded

Names close to this product, grouped by how directly they compete:

| Name | What it is | Why it matters here |
| --- | --- | --- |
| [Agentrove](https://github.com/Mng-dev-ai/agentrove) | Self-hosted workspace for running Antigravity, Claude Code, Codex, Copilot, Cursor, Grok and OpenCode from one interface; Apache-2.0 | Same audience and same "one interface for agents" promise. The README documents a macOS Tauri build and an iOS bundle id `com.agentrove.desktop`. |
| [AgentQuilt](https://agentquilt.dev/) | Git-native framework that compiles agent instructions into `AGENTS.md`, `.claude/agents` and Agent Skills with a drift-checking lock file | Directly overlaps rules, skills and drift detection. Uses the `agent` + craft-object pattern. |
| [Agentry](https://agentryco.com/) | "One workspace for every coding agent"; native, local-first Tauri desktop app for Claude Code, Codex, Kiro, OpenCode and Hermes | Same product category and the same local-first desktop framing. See also [npm agentry](https://www.npmjs.com/package/agentry) and [docs.agentry.run](https://docs.agentry.run/getting-started/welcome). |
| [Cairn](https://github.com/cairn-framework/cairn) | Framework for organising project context and instructions for Claude Code, Cursor and Codex | Close on rules and context; the word is also used by [cairn-dev/cairn](https://github.com/cairn-dev/cairn) and [cairn.computer](https://cairn.computer/). |
| [Plinth](https://plinth.fun/) | Registry for installing MCP tools; [jabrena/plinth](https://github.com/jabrena/plinth) is an AI engineering toolkit built around commands, agents, skills and MCP servers | MCP and skills overlap; the architectural metaphor does not survive contact with the market. |
| [Codicil](https://www.npmjs.com/package/codicil-cli) | Markdown project knowledge and review workflows for coding agents; [colehellman/codicil](https://github.com/colehellman/codicil) indexes docs for MCP clients | Rules and context overlap. |
| [Selvage](https://github.com/selvage-lab/selvage) | CLI and MCP server for AI-assisted code review | Adjacent developer tooling; the name is also used commercially. |
| [Quoin](https://www.quoin.ai/) | Agentic research platform; also [@agent-ix/quoin](https://www.npmjs.com/package/@agent-ix/quoin) on npm | Agent-space collision. |
| [ScopeStead](https://scopestead.com/) | Preconstruction software for bids and takeoff | Different category, but occupies the exact `Scope*` spelling pattern. |
| [Scopefold](https://scopefold.dev/) | Scope management for product and engineering teams, in development; also an Android subscriptions app | Different category, same coined shape. |
| [ScopeHold](https://github.com/ScopeHold/scopehold-mcp) | `scopehold-mcp` and `scopehold-cli` on GitHub | MCP tooling under the same prefix. |
| [ScopeLane](https://scopelane.com/) | AI call answering and booking for home-service businesses | Different category, same prefix. |
| [AgentFold](https://github.com/279458179/agentfold) | "Virtual memory for AI agents" | Same `agent` + verb pattern. |

These searches found several active `agent`, `scope`, `grove`, `wright`, `fold`
and `context` names in or near the category. Similarity does not by itself
establish a legal conflict.

## Candidate screening

| # | Candidate | Origin | Evidence found | Relevance | Reading |
| --- | --- | --- | --- | --- | --- |
| 1 | **Agentrove** | Supplied | GitHub `Mng-dev-ai/agentrove`, Apache-2.0, 340 stars, pushed 2026-11-04; README describes a self-hosted workspace for seven coding agents with Docker, macOS Tauri and iOS builds. `agentrove.com` resolves behind Cloudflare (HTTP 403 to a non-browser request). No unscoped npm package. | **Exact category** | Reject. Name and positioning are already taken by a shipping product. |
| 2 | **Agentry** | Supplied | `agentryco.com`: "One workspace for every coding agent", local-first native desktop, macOS and Windows. `agentry.to`: email infrastructure for AI agents. `docs.agentry.run`: run agent-built apps on your own server. npm `agentry`: "Compose and reuse your AI agents like React components", with `agentry-mcp`, `agentry-watch`, `opencode-agentry`. `agentry.com` and an iOS app also exist. | **Exact category plus repeated word** | Reject. The word is used by several agent products at once; there is no clean signal. |
| 3 | **Scopewright** | Supplied | `scopewright` on npm: version 0.3.0, first published 2026-10-07, modified 2026-11-04, maintainer `jwmarshall`. It scaffolds read-only reviewers for Claude Code, Codex, Pi, OhMyPi, Antigravity CLI, Gemini CLI, OpenCode and Cursor. Its managed CLI writes `.claude/skills/`, `.codex/agents/`, `.omp/skills/` and shared `.agents/skills/`, records SHA-256 hashes and ownership in `.scopewright/install.json`, and rejects conflicts. Also `scopewright.app` (contractor estimating) and `scopewright.aoapps.io`. | **Exact category, and functional overlap with AgenTuna's installer model** | Reject. This is the strongest collision in the set. |
| 4 | **Configrove** | Supplied | npm registry document: 404; npm search index: 0. GitHub repository search: 0; user and organisation: 404. WHOIS for `configrove.com`: no matching record on 2026-11-04. No A record for `.com`, `.dev`, `.io` or `.app`. Web search returned only unrelated near-misses (Configo, configurate, confroid). | **No product found** | Weakest distinctiveness of the survivors. `config` is generic and `grove` is the Agentrove morpheme. |
| 5 | **Scopary** | Supplied | npm registry: 404; npm search: 0. GitHub repositories: 0; user and organisation: 404. No software product found; the spelling appears as an archaic botanical word in Culpeper's herbal and as a GIF tag, with near-misses Scopuly and Scoutary. `scopary.com` is registered and serves an OVH "Site en construction" parking page. | **No product found** | Clean slate in the category, but the obvious `.com` is held and the word is obscure. |
| 6 | **Configfold** | Invented (config + fold) | npm registry: 404; npm search: 0. GitHub exact-name repositories: 0, nearest are `ConfigFolder`-style repositories. WHOIS for `configfold.com`: no matching record on 2026-11-04. Web search: no product. | **No product found** | Clear but clunky; reads as "config folder" and is hard to say aloud. |
| 7 | **Scopefold** | Invented (scope + fold) | `scopefold.dev` is a scope-management product in development; a Google Play app "Scopefold: Subscriptions" exists; `scopefold.com` was registered 2026-04-06; two near-name GitHub repositories. npm: 404. | **Different category, same coinage** | Reject. The coinage is already in use and the `.com` is taken. |
| 8 | **Scopestead** | Invented (scope + stead) | `scopestead.com` is ScopeStead preconstruction software; npm: 404; GitHub exact-name repositories: 0. | **Different category, same coinage** | Reject. Same problem as Scopefold. |

Additional names tested and rejected before reaching the table: **Copse** (Copse
is a published font family, `@fontsource/copse`, so search is dominated by the
typeface; `copse.com` registered since 2000), **Grovewright** (zero npm and
GitHub hits, but it combines the two most contested morphemes in this category
and would read as a Scopewright variant), **Configweave** (the GitHub
organisation `Configweave` exists and owns `wscript`, a Rust-flavoured scripting
language; different category, but it pre-empts the `Config*` compound family),
**Codicil**, **Plinth**, **Selvage**, **Quoin**, **Cairn**, **ScopeLane** and
**ScopeHold** (all have active products or projects, several in agent tooling).

## Shortlist assessment

### 1. Configrove

- **For:** No npm package, GitHub project, WHOIS record or indexed product.
  Reads as "configuration" plus "grove"; easy to spell from hearing.
- **Against:** `config` is descriptive and `grove` is the same morpheme as
  Agentrove, an exact-category competitor with a shipping desktop app. Search
  results and word of mouth may cluster the two. The `.com` check returned no
  record but that is a registry observation on one date, not a reservation.
- **Use if:** the team wants a descriptive working title and accepts potentially
  weak distinctiveness.

### 2. Scopary

- **For:** The only supplied name with no software product found under the exact
  spelling. It reads as `scope` + `-ary`, which fits the product's scope model.
- **Against:** `scopary.com` is registered and parked, so the natural domain is
  not free. The word is obscure and its pronunciation is not obvious on sight.
  `Scope*` is a crowded prefix in software (ScopeStead, Scopefold, ScopeLane,
  ScopeHold, Scopewright).
- **Use if:** the team wants a coined name with category meaning and is willing
  to use a different TLD or a compound domain.

### 3. Configfold

- **For:** The cleanest search result of any candidate tested: no package, no
  repository, no product, no `.com` WHOIS record. `fold` matches the product's
  scope-layering model.
- **Against:** It sounds like "config folder", which invites the wrong mental
  model, and the consonant cluster is awkward in speech. `fold` is a generic
  suffix.
- **Use if:** the team prioritises a clear search space over sound.

## Recommendation

Do not commit to any of these names yet. Two concrete options:

1. **Working title: Configrove.** Use internally while a formal search runs.
   Pair it with the descriptor, as in "Configrove — agent config studio",
   so the category is legible even if the name is later replaced.
2. **If distinctiveness matters more than descriptiveness: Scopary.** Budget for
   a non-`.com` primary domain and test the pronunciation with target users
   before printing anything.

The limited searches do not establish that any candidate is available or that
other short, meaningful names would necessarily conflict.

## Next steps

1. Run a formal clearance search: USPTO and EUIPO registers, company
   registries, app stores, social handles, npm/PyPI/crates.io, and the `.com`,
   `.dev`, `.app` and `.io` domains.
2. Say each shortlisted name aloud to five target users and ask them to spell it
   back. Scopary and Configfold are the likely failures.
3. Reserve domains and handles only after clearance, and only for the final
   choice.
4. Record the final decision and its rationale in this file, with the date of
   the clearance search.

## Sources

Primary sources used for the conflict findings:

- Agentrove: <https://github.com/Mng-dev-ai/agentrove>
- Agentry desktop: <https://agentryco.com/>
- Agentry docs: <https://docs.agentry.run/getting-started/welcome>
- Agentry email: <https://www.agentry.to/docs>
- Agentry npm: <https://www.npmjs.com/package/agentry>
- Scopewright npm: <https://www.npmjs.com/package/scopewright>
- Scopewright repository: <https://github.com/jwmarshall/scopewright>
- Scopewright contractor product: <https://www.scopewright.app/>
- AgentQuilt: <https://agentquilt.dev/>
- AgentFold: <https://github.com/279458179/agentfold>
- Cairn framework: <https://github.com/cairn-framework/cairn>
- Plinth MCP registry: <https://plinth.fun/>
- Plinth Java toolkit: <https://github.com/jabrena/plinth>
- Codicil MCP: <https://github.com/colehellman/codicil>
- Selvage: <https://github.com/selvage-lab/selvage>
- Quoin: <https://www.quoin.ai/>
- ScopeStead: <https://scopestead.com/>
- Scopefold: <https://scopefold.dev/>
- Scopefold Android app: <https://play.google.com/store/apps/details?id=com.hinomarulabs.scopefold>
- ScopeHold MCP: <https://github.com/ScopeHold/scopehold-mcp>
- ScopeLane: <https://scopelane.com/>
- Scopary parking page: <https://scopary.com/>
- Scopary botanical usage: <https://commons.wikimedia.org/wiki/File:Culpeper%27s_English_physician_;_and_complete_herbal_(IA_b22007908).pdf>
- Configweave organisation: <https://github.com/Configweave/wscript>
- npm registry and search API: <https://registry.npmjs.org/>
- GitHub search API: <https://api.github.com/search/repositories>
- WHOIS lookup: <https://lookup.icann.org/en/lookup?q=configrove.com>

Registry responses (npm 404/200, GitHub counts, WHOIS records, DNS answers)
were observed on 2026-11-04 and can change.
