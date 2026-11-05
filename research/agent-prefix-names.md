# Agent-prefixed name research: safe harbor, organized home, stewardship

Research date: **2026-11-04**. This is name screening, not trademark, company-name
or legal clearance. Nothing below establishes that a name, domain or handle is
available, registrable or safe to launch. No product was installed, renamed or
reconfigured; no file outside this report was changed.

This report covers a second, narrower question than
[product-name-research.md](product-name-research.md): Agent-prefixed names
evoking a safe harbor, an organized home, coordination or stewardship. AgentHarbor
is an existing product and is excluded; Agentrove was rejected in the earlier
report and is not revisited here.

## Bottom line

The obvious names are already in use in agent tooling. Nearly every short,
familiar harbor or home word tested has an existing product, package or both.
Fourteen of them:
**AgentHarbor, AgentHaven, AgentBerth, AgentCove, AgentQuay, AgentWharf,
AgentNook, AgentRoost, AgentStead, AgentKeep, AgentPerch, AgentProvost,
AgentQuartermaster** and **AgentBosun**. Several are direct functional
competitors, not just name collisions.

Five candidates survived every exact check run here:

| Rank | Candidate | Why it survives | Main weakness |
| --- | --- | --- | --- |
| 1 | **AgentFarmstead** | No exact package, repository, product or .com record. Familiar words, four syllables, reads as a working home. | "Farm" may read rural; no near-name product found, but the word is generic. |
| 2 | **AgentSteading** | No exact package, repository, product or .com record. Most distinctive of the group. | Rare word; one morpheme from the shipping products AgentStead (agentstead.sh, agentstead.com). |
| 3 | **AgentHomestead** | No exact package, repository, product or .com record. Instantly understood. | Generic; Homestead AI is a near name in the same broad category; heavy non-software brand noise. |
| 4 | **AgentCastellan** | No exact package, repository, product or .com record. Strong stewardship metaphor. | Five syllables; Castellan (castellan.so) is a near name; fantasy tint. |
| 5 | **AgentBoatyard** | No exact package, repository, product or .com record. Concrete harbor metaphor. | Boatyard (boatyard.com) is an established marine-software brand; "yard" implies repair. |

**Recommendation for the next step: AgentFarmstead.** It keeps the shape and
cadence of AgentHarbor — Agent plus a concrete sheltered place, four syllables,
no consonant pile-up — while returning no exact hit in any source checked.
AgentSteading is the strongest fallback if the rural "farm" association is
unwanted. These are candidates for a professional clearance search, not cleared
names.

## Additional short-list pass

An independent follow-up considered shorter-sounding place and navigation names.
On 2026-11-04, exact **unhyphenated** npm, PyPI and crates.io package endpoints
returned HTTP 404 for each name below. Exact-phrase web searches did not reveal
a matching product in their returned results. This pass did not test hyphenated
packages, GitHub, trademark registers, app stores, handles, or domain registration.
Absence from search results and registry 404s do not establish availability.

| Candidate | Meaning and trade-off | Known nearby name |
| --- | --- | --- |
| **AgentInlet** | A sheltered entrance; closest to the harbor theme. | [Inlet](https://www.inletai.io/) is an AI agent infrastructure SDK. |
| **AgentPortico** | A welcoming entrance or covered space; distinct visual identity. | [AgentPort](https://docs.agentport.sh/) and [Portico](https://runportico.com/) are AI tools. |
| **AgentSlipway** | A place where vessels enter the water; distinctive, less familiar word. | [AgentWay](https://agentwayai.com/) is an AI agent control plane. |
| **AgentWaypoint** | An organizing point along a journey; longer and suggests navigation. | [Waypoint](https://docs.getwaypoint.dev/) serves AI coding workflows. |

Independent preference: **AgentInlet** for a compact harbor-adjacent name;
**AgentPortico** for a more distinctive look and feel. Both need fuller
conflict and registration searches before selection.

## How to read the evidence

| Label | Meaning | What it does not mean |
| --- | --- | --- |
| **No hit** | The exact spelling returned nothing in the sources under Method. | Availability. Unindexed products, private repos, app-store listings and common-law marks do not appear there. |
| **RDAP 404** | Verisign's .com registry returned no registration record for the exact domain at query time. | A reservation, a price, or a promise that the name can be bought. Premium and reserved names exist. |
| **Registered** | A registration record exists. | That a product is behind it; many are parked. |
| **Package hit** | An exact npm, crates.io or PyPI package exists. | Quality, ownership or intent; download counts were not assessed. |
| **Near name** | A different exact name is phonetically or visually close, or uses the same distinctive morpheme in the same market. | A legal conflict. Similarity alone is not infringement. |

A clean row below means "nothing found", never "available".

## Method

Every candidate in the screening set was checked on 2026-11-04 with:

| Check | Endpoint or command | Signal used |
| --- | --- | --- |
| npm exact | `https://registry.npmjs.org/<pkg>` for `agent-<word>` and `agent<word>` | HTTP 200 = package exists |
| crates.io exact | `https://crates.io/api/v1/crates/<pkg>` for `agent-<word>` and `agent_<word>` | HTTP 200 |
| PyPI exact | `https://pypi.org/pypi/<pkg>/json` for `agent-<word>` and `agent<word>` | HTTP 200 |
| GitHub repository | `https://api.github.com/search/repositories?q=<name>+in:name` | `total_count` and top hits |
| GitHub handle | `https://github.com/<name>` | 404 = no user or organisation page |
| DNS | `dig +short A agent<word>.<tld>` for .com, .dev, .io, .app, .so, .sh | An A record means something is published |
| .com registry | `https://rdap.verisign.com/com/v1/domain/<domain>` | 404 = no registration record |
| Web | exact-phrase searches listed in the search log | Indexed product, post or repository |
| First party | page fetch for every conflict treated as material | Product description |

Not searched: USPTO, EUIPO, UKIPO or any other trademark register; company
registries; app stores; social handles; paid clearance databases; registries
other than npm, crates.io and PyPI. .dev, .io and .app registration status was
**not** resolved: only DNS A records were checked, and a missing A record is not
evidence that a domain is unregistered. `whois` returned no structured output
for those TLDs on this machine. npm organisation lookups returned HTTP 403
without authentication and are not used as evidence. GitHub's unauthenticated
search limit is 10 requests per minute; batches were spaced accordingly.

## The waterfront is already full

### Harbor and mooring

| Name | Evidence | Read |
| --- | --- | --- |
| [AgentHarbor](https://agent-harbor.dev/) | "Long horizon agent orchestration layer"; deploy, observe and debug fleets in sandboxes. [Docs](https://docs.agent-harbor.com/) describe a TUI, a C-Suite Manager Agent, multi-host fleet and the Agent Control Protocol. npm [`agentharbor`](https://www.npmjs.com/package/agentharbor) 0.1.1 is an x402 client; npm `agent-harbor` was created 2026-03-06. | Exact category. Existing product; excluded. |
| [AgentHaven](https://agenthaven.org/) | "Private messaging network for AI agents ... not for humans"; [manager/agenthaven](https://github.com/manager/agenthaven), JavaScript, MIT per site metadata, 0 stars, created 2026-09-29. Also listed as a skill on agensi.io. | Exact name in agent tooling. Reject. |
| [AgentBerth](https://docs.rs/crate/agent-berth/3.3.0) | Crate `agent-berth` 3.3.0 on docs.rs. [Skill Harbor](https://github.com/johntimothybailey/skill-harbor) (TypeScript, created 2026-03-18) calls itself a "Declarative Workspace Orchestrator for Agent Skills ... Sync & Governance Engine", which directly overlaps skills, sync and governance. A DeepWiki page for Skill Harbor's TUI, returned by search but not independently re-fetched, indexes an `AgentBerth` target selector. | Exact name plus direct functional overlap. Reject. |
| [AgentWharf](https://github.com/winghv/AgentWharf) | Go, Apache-2.0, created 2026-06-12, pushed 2026-10-29: "Open-source session gateway for coding agents: BYOM, durable replay, multi-client sync, ACP bridge". | Exact name, same audience. Reject. |
| [AgentCove](https://agentcove.ai/) | "Your Personal AI Agent, Ready in Minutes". | Exact name in agent tooling. Reject. |
| [AgentQuay](https://docs.agent.tech/introduction/product-overview) | agent.tech ships AgentQuay, a public directory of agents transacting on x402 rails. Separately, npm and PyPI [`agentquay`](https://pypi.org/project/agentquay/) 0.0.1 (npm record created 2026-10-11) is a "Local workbench shell where projects dock in as plugins ... operated by an AI assistant". | Two products; the package is a local workbench, a functional neighbour. Reject. |
| [AgentSlipway](https://slipway.sh/) | Slipway sells preview environments and a first-party MCP server that lets Claude Code, Cursor or Claude Desktop bring up environments, read logs and run commands. | Morpheme in active agent tooling; "slip" is a poor signal for a safety-first product. Reject. |
| AgentJetty | No exact package, repository or product found. Near name: [Agentty](https://www.agentty.run/), "The AI Native Terminal to Orchestrate Coding Agents". | Shares the Agentty word shape and ending. High visual confusion risk. |
| [AgentMooring](https://mooring.sh/about) | Mooring is an existing product. | Ordinary noun plus shipping product. Reject. |
| AgentDock | npm `agent-dock` and `agentdock` exist; agentdock.com and agentdock.io resolve. | Package and domain occupied. Reject. |
| AgentPier | No exact package; agentpier.com and agentpier.dev resolve. | Domain occupied, product unverified. Avoid. |
| AgentMoor | agentmoor.com registered 2026-09-05, with HiChina nameservers. | Meaning is ambiguous in English (moorland versus to moor a vessel). Avoid. |

### Home, nesting and shelter

| Name | Evidence | Read |
| --- | --- | --- |
| [AgentNook](https://www.agentnook.com/) | Two live products: a real-estate CRM workspace with an AI assistant, and [AgentNook for Mac](https://agentnook.dev/), a $9 app that shows Claude Code context-window usage in the notch. | Two exact products. Reject. |
| [AgentRoost](https://agentroost.app/en/) | Managed AI-agent hosting for Hermes and OpenClaw, from $19.99 per agent per month. | Same category. Reject. |
| [AgentStead](https://agentstead.sh/) | External identity for AI agents: inbox, browser identity, credentials, MFA, activity history. [agentstead.com](https://www.agentstead.com/) separately sells always-on agent hosting from $9/month. | Two exact products. Reject. |
| [AgentHearth](https://ourhearth.ai/) | Hearth is used by several AI products: ourhearth.ai (family AI workspace), [hearthengine.com](https://hearthengine.com/) (agent engine with a "bring your own agent" folder) and a documented hearth agent engine. | Crowded morpheme in exactly the agent workspace space. Reject. |
| [AgentPerch](https://buildwithperch.com/) | Perch is crowded here: a Windows "command center for coding agents"; [ReScienceLab/Perch](https://github.com/ReScienceLab/Perch), a macOS session manager for Claude Code, Codex and Pi; [ik-labs/perch](https://github.com/ik-labs/perch), a menu-bar session watcher; and a fourth Perch that lives in the Mac notch. | Direct category collision. Reject. |
| AgentAlcove | No software product found. The exact phrase is used as a Writer's Digest column heading ("AGENT ALCOVE") and agentalcove.com is registered, with Spaceship nameservers. | Weak but occupied, with an exact-phrase print use. Avoid. |
| AgentBurrow | Burrow is used by four agent products: burrow.run (security platform for AI agents), burrow.team (social network for AI agents), useburrow.dev (MCP localhost tunnels) and Array-Ventures/burrow (multi-agent CRM). | Crowded morpheme. Reject. |
| AgentNest | npm and PyPI `agentnest`; agentnest.com, .io and .app resolve. | Package and domains occupied. Reject. |
| AgentCoop | npm and PyPI `agentcoop`; agentcoop.com resolves. | Package and domain occupied. Reject. |
| AgentHome | npm `agenthome`; agenthome.com, .dev, .io and .sh resolve. | Package and domains occupied. Reject. |
| AgentHouse | npm `agent-house`; agenthouse.com and .app resolve. | Package and domains occupied. Reject. |
| AgentLanding | npm `agent-landing`; agentlanding.com resolves. | Package and domain occupied. Reject. |
| AgentCommons | npm and PyPI `agent-commons`; agentcommons.com, .io and .app resolve. | Package and domains occupied. Reject. |
| AgentHutch | No package or repository found. agenthutch.com registered 2026-10-18 behind WHOIS privacy and resolves to 127.0.0.1; agent-hutch.com is unregistered. | Domain occupied; the 127.0.0.1 record suggests a local development host, not a product. Treat as occupied, unverified. |

### Stewardship and coordination

| Name | Evidence | Read |
| --- | --- | --- |
| [AgentSteward](https://pypi.org/project/agent-steward/) | PyPI `agent-steward` 0.19.1: "A quality gate and spend auditor for your AI agent fleet", with a GitHub repository. | Exact package in agent tooling. Reject. |
| AgentWarden | npm and PyPI `agentwarden`; agentwarden.com and .io resolve. | Package and domains occupied. Reject. |
| AgentKeeper | npm and PyPI `agentkeeper`; [Thinklanceai/agentkeeper](https://github.com/Thinklanceai/agentkeeper) (123 stars) is an AI-memory product. | Exact package and product. Reject. |
| [AgentKeep](https://agentkeep.com/) | agentkeep.com is a marketplace for AI agents. GitHub has Finner1909/agentkeep (background command keepalive), ZioSec/agentkeep (sandbox for AI agents) and marfillaster/agentkeep (shared context for coding agents). | Crowded. Reject. |
| [AgentProvost](https://github.com/CharmingSteve/agent-provost) | 3 stars, created 2026-03-21: "Compliance firewall for Alpaca MCP. Every call must register. Every action is logged. Kill-switch for any rule breach." | Exact agent-space collision. Reject. |
| [AgentQuartermaster](https://www.quartermaster.cloud/docs) | "A local cross-tool registry, converter, and lifecycle manager for AI-agent context", exposed as an MCP server; [Agent Update](https://www.tryagentupdate.com/docs/agents/quartermaster) also ships a Quartermaster agent. | Closest functional description to this project found in this research. Reject. |
| [AgentBosun](https://jkarancsi.com/blog/cutting-agent-cost/) | A published evaluation of "AgentBosun" describes routing coding-agent tasks across model tiers, escalating when needed. | Exact name in agent tooling. Reject. |
| AgentRegistrar | npm `agentregistrar`; agentregistrar.com, .dev and .io resolve. | Package and domains occupied. Reject. |
| [AgentGrange](https://grangelabs.com/) | No exact AgentGrange product, but Grange Labs sells hiring and assigning specialist AI agents, and The Grange is a large insurance brand. | Near name in the same market, plus heavy noise. Avoid. |
| [AgentQuarters](https://linktr.ee/agentquarters) | The exact name is used by a real-estate transaction-coordination business; agentquarters.com is registered. | Exact business name; avoid. |
| AgentPurser | No product or package found. agentpurser.com registered 2026-10-11, with registrar-servers.com nameservers. | Good maritime-stewardship metaphor, weak recognition, domain taken. |
| AgentReeve | No product or package found. agentreeve.com registered 2026-07-22, with GoDaddy nameservers. | Short and distinctive, archaic word, domain taken. |
| [AgentHarbormaster](https://harbormaster.ai/) | No exact product. Harbormaster AI is maritime technology; [Harbormaster](https://docs.crewship.ai/guides/harbormaster) at crewship.ai is an agent approval gate. | Near name in agent infrastructure. Avoid. |
| AgentPortico | No package or product. agentportico.com and agent-portico.com are unregistered; GitHub has one repository with no description, `Philosophiser/teams-agent-portico` (TypeScript, 0 stars, created 2026-01-16). Near AgentPort and Portico AI. | Thin slate; repository use is not a product. |
| AgentInlet | No package, repository or product. agentinlet.com is unregistered; agent-inlet.com is registered. Near Inlet AI. | Thin slate; near-name risk. |

## Clean candidates

All rows below returned no exact package (npm, crates.io, PyPI), no exact
repository under `<name> in:name`, no exact indexed product, and both the joined
and hyphenated .com forms returned RDAP 404.

| Candidate | Metaphor | .com status | Syllables | Notes |
| --- | --- | --- | --- | --- |
| **AgentFarmstead** | A farm and its buildings; a self-contained working home | `agentfarmstead.com` and `agent-farmstead.com` 404 | 4 | No near-name product found. Only a descriptive use by an AI collective on The Colony. |
| **AgentSteading** | A farmhouse with barns, stables and cattle sheds; a farmstead or homestead | both 404 | 4 | Most distinctive. Near name AgentStead is a shipping product, one morpheme away. |
| **AgentHomestead** | A home and its land | both 404 | 4 | Instantly understood. [Homestead AI](https://github.com/Gavril242/Homestead-AI) is a 1-star multi-agent engineering platform; Homestead is also a website builder and a legal term. |
| **AgentCastellan** | The governor or caretaker of a castle or keep | both 404 | 5 | Clean exact. Castellan (castellan.so) is an AI property-tax service; the word also carries a fantasy/gaming tint. |
| **AgentBoatyard** | A place where boats are built and repaired | both 404 | 4 | Clean exact. [Boatyard](https://boatyard.com/business/) is established marine-industry software. |
| **AgentAnchorage** | A harbor area where ships anchor | both 404 | 5 | Clean exact, but Anchorage, Alaska dominates search results. |
| **AgentQuayside** | The quay side of a harbor | both 404 | 4 | Clean exact. [Quayside](https://autotelica.ai/quayside) is an AI-agent governance product — a near name in the same broad market. |
| **AgentHarborage** | Shelter for a vessel | both 404 | 5 | Clean exact, but the word is rare and reads awkwardly. |

Definitions from [dictionaryapi.dev](https://api.dictionaryapi.dev/api/v2/entries/en/steading)
(Wiktionary-derived): steading, "a farmhouse and outer buildings such as barns,
stables, cattle-sheds, etc.; a farmstead; a homestead"; castellan, "the governor
or caretaker of a castle or keep"; anchorage, "a harbor, river, or offshore area
that can accommodate a ship at anchor"; boatyard, "a place where boats are built
and repaired"; harborage, "a place for refuge for a vessel"; purser, "the person
responsible for handling the accounts on a ship"; reeve, "any of several local
officials, with varying responsibilities".

## Shortlist, ranked

### 1. AgentFarmstead

A farmstead is the farm and its buildings: a self-contained, working home. That
is the product's model almost exactly — a central folder per scope, distributed
to each tool's own folders, with backups and drift detection. It is four
syllables, spelled as it sounds, and has no awkward consonant join.

Evidence: no exact package, repository, product or .com registration. The only
web use found was descriptive, in a profile of an AI collective building on a
farmstead in Indiana ([The Colony](https://thecolony.cc/directory)).

Risks: "farm" may read agricultural to a developer-tool audience; the word is
generic, so trademark clearance may be narrow; a premium .com price is possible
even though no record exists. Mitigation: keep the mark as AgentFarmstead rather
than "Farmstead" alone.

### 2. AgentSteading

"Steading" is the Scots and Northern English word for the farmhouse and its
outbuildings. It is the most distinctive survivor: no exact product uses it.

Risks: four syllables, but many readers will not know the word, so it needs a
one-line explanation; and it sits one morpheme from **AgentStead**, which is
already two shipping products (agentstead.sh for agent identity, agentstead.com
for agent hosting). In speech, "Agent Steading" and "Agent Stead" will be
confused. That is the only reason it is not ranked first.

### 3. AgentHomestead

The most immediately legible option: everyone knows what a homestead is, and the
word carries home, land and self-sufficiency. Four syllables, no spelling risk.

Risks: generic; Homestead AI is an existing autonomous multi-agent engineering
platform with the same broad audience; Homestead.com is a long-established
website builder; "homestead" is also a legal term. Search results would be
dominated by non-software uses.

### 4. AgentCastellan

A castellan is the keeper of a castle: stewardship, custody and a fortified
home. It is distinctive and has no exact package, repository, product or .com
record.

Risks: five syllables; Castellan (castellan.so) is an AI property-tax appeal
service; the word may read fantasy or game-like to some developers; spelling
(Castellan versus Castilian) is less obvious than the top three.

### 5. AgentBoatyard

A boatyard is where vessels are kept, maintained and launched: the harbor
metaphor without the crowded "harbor" word. Four syllables, concrete and
friendly.

Risks: Boatyard (boatyard.com) is an established marine-industry software brand,
so the mark is not distinctive on its own; "yard" can imply repair or overhaul;
the compound is slightly prosaic.

## Screened and set aside

| Candidate | Reason |
| --- | --- |
| AgentAnchorage | Clean evidence, but Anchorage, Alaska dominates search and the name reads like a place or an insurance agency. Five syllables. |
| AgentQuayside | Clean exact, but Quayside is already an AI-agent governance product, so the distinctive morpheme is taken. |
| AgentHarborage | Clean exact, but rare and clunky; "harborage" is not a word most readers know. |
| AgentGrange | .com free and no exact product, but Grange Labs sells AI-agent staffing and The Grange is a large insurer. |
| AgentPurser | No product found, but the domain is registered and the word is obscure; good metaphor, weak recall. |
| AgentReeve | No product found, but the domain is registered and the word is archaic. |
| AgentHarbormaster | Clean registries, but Harbormaster is already used by an agent approval product and a maritime AI company; six syllables. |
| AgentHutch | Domain occupied; no product verified. |
| AgentCote | Domain free and no package, but pronunciation is ambiguous ("coat" versus "cot") and the word is obscure. No exact web search was run for this spelling. |
| AgentBillet | Domain free and no package, but "billet" is obscure and ambiguous. No exact web search was run for this spelling. |
| AgentPortico | Thin slate; one repository with no description, near names AgentPort and Portico AI. |
| AgentInlet | Thin slate; near name Inlet AI; hyphenated .com registered. |

## Near-name risk

Adding "Agent" does not create distance when the second word is already a brand.
The traps found here:

- **AgentJetty vs Agentty** — the same Agent-plus-tty silhouette, same category
  (terminal for orchestrating coding agents). Do not use.
- **AgentHaven vs AgentHarbor** — both exist in agent tooling and both begin
  `AgentHa-`. A third name in that shape would be read as part of the family.
- **AgentSteading vs AgentStead** — the strongest near-name problem among the
  survivors. AgentStead is a shipping identity product and a shipping hosting
  product.
- **AgentHarbormaster vs Harbormaster** — Harbormaster is an agent approval gate
  at crewship.ai and a maritime AI brand.
- **AgentCastellan vs Castellan** — Castellan is an AI property-tax service.
- **AgentGrange vs Grange Labs** — Grange Labs sells AI-agent staffing.
- **AgentQuayside vs Quayside** — Quayside is AI-agent governance.
- **AgentHomestead vs Homestead AI** — a multi-agent engineering platform.
- **AgentBoatyard vs Boatyard** — established marine software.
- **AgentBosun** — a published coding-agent routing experiment uses the name.

## Search log

Exact queries run through the web search tool, in order. Results are summarised
in the sections above.

```text
"AgentNook" software app
"AgentRoost"
"AgentStead"
"AgentReeve"
"AgentBerth"
"AgentHaven"
"AgentSlipway" OR "Agent Slipway"
"AgentBoatyard" OR "Agent Boatyard"
"AgentQuayside" OR "Agent Quayside"
"AgentBosun" OR "Agent Bosun"
"AgentCastellan" OR "Agent Castellan"
"AgentProvost" OR "Agent Provost"
"AgentHarbormaster" OR "Agent Harbormaster"
"AgentAnchorage" OR "Agent Anchorage"
"AgentHomestead" OR "Agent Homestead"
"AgentSteading" OR "Agent Steading"
"AgentRoadstead" OR "Agent Roadstead"
"AgentQuartermaster" OR "Agent Quartermaster"
"AgentGrange" OR "Agent Grange"
"AgentHearth" OR "Agent Hearth"
"AgentAlcove" OR "Agent Alcove"
"AgentPerch" OR "Agent Perch"
"AgentPurser" OR "Agent Purser"
"agentsteading"
"agenthomestead" OR "agent-homestead"
"AgentQuarters" OR "Agent Quarters" AI
"AgentKeep"
"AgentBurrow" OR "Agent Burrow"
"Grange" AI agent software product
"agentquayside" OR "Agent Quayside" software
"AgentHarborage" OR "Agent Harborage"
"AgentFarmstead" OR "Agent Farmstead" OR "Farmstead AI" agents
```

Registry screening covered 70+ words, including: harbor, haven, anchorage,
berth, quay, moor, mooring, buoy, pier, dock, slip, slipway, quayside, boatyard,
dockyard, jetty, roadstead, harborage, dockage, hearth, homestead, farmstead,
steading, nest, roost, coop, perch, hutch, cabin, alcove, cote, bight, nook,
quarters, billet, grange, manor, lodge, burrow, keep, ward, steward, warden,
keeper, custodian, provost, purser, reeve, castellan, seneschal, bosun,
coxswain, quartermaster, harbormaster, registrar, concierge, wrangler, commons,
home, house, landing, portico, inlet. Each was checked for `agent-<word>` and
`agent<word>` packages and for DNS A records on .com, .dev, .io and .app. RDAP
for both .com forms was run for the shortlist, the set-aside names and the main
rejects named in the tables above.

## Limitations

1. **No trademark search.** USPTO, EUIPO, UKIPO and other registers were not
   queried. Registered or pending marks, common-law rights and opposition risk
   are unknown.
2. **No company-registry or app-store search.** A company name or an app listing
   can exist without an indexed web page.
3. **No social-handle search.** GitHub user pages were checked only for the
   shortlist; X, LinkedIn, Discord, Reddit and YouTube were not.
4. **.dev, .io and .app remain unresolved.** Only DNS was checked. A 404 from the
   Verisign RDAP endpoint applies to .com only and was not used for other TLDs.
5. **Index coverage is incomplete.** Very new products, private repositories,
   invite-only betas and products without a marketing site will not appear.
6. **The space moves weekly.** Several conflicts found here were created in
   September and October 2026. Any result can change before launch.
7. **Similarity is not conflict.** Near names are flagged as risk, not as legal
   conclusions.

## Next steps

1. Commission a professional clearance search for **AgentFarmstead**
   (word mark, class 9 and class 42, US and EU at minimum).
2. Check `agentfarmstead.com` at a registrar for premium pricing and reserved
   status before relying on the RDAP 404.
3. Check the handle set: GitHub organisation, npm scope, X, LinkedIn, Discord,
   YouTube, and the macOS app bundle identifier.
4. Run a five-person pronunciation and spelling test: say the name once, ask each
   person to type it, and count the errors.
5. Keep **AgentSteading** as the fallback only if the AgentStead confusion is
   acceptable after the pronunciation test.
