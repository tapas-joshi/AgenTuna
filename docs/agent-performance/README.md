# Subagent performance log

Tracks which model and reasoning effort each build subagent ran with, what the run cost, and how
the result held up under the integration gates.

| File | Source |
|---|---|
| `runs.csv` | Generated: one row per agent (provider, model, reasoning effort, minutes, turns, tool calls, tokens, cost, quota stop). |
| `ratings.csv` | Hand-written: `agent,grade,note`. The only file to edit by hand. |
| `summary.md` | Generated: runs grouped by model and reasoning effort, plus a per-wave view, joined with the grades. |

## Grades

Assigned by the lead after the integration gates (fmt, clippy, tests, docs, UI, Playwright), not from the agent's own report.

| Grade | Meaning |
|---|---|
| A | Every assigned story implemented and verified; the report matched what the gates showed. |
| B | Mostly done; minor gaps or needed one push. |
| C | Partial, or needed a re-run, or left failing tests or compile errors. |
| D | Quit early, broke the build for others, or reported unverified work. |
| Q | Cut off by a usage quota or rate limit. Not scored. |

## Update after a wave

1. Add one `agent,grade,note` row per new agent to `ratings.csv`.
2. Run `python3 scripts/agent-stats.py` (it finds the newest omp session folder, or pass the folder as an argument).
3. Commit the three files.

## Reading the data

- "Reasoning" comes from the `thinking_level_change` events in each agent's session log. Runs with no such event show `unrecorded` (the harness default at the time).
- Agents revived by a message keep the model they started on unless the setting was changed before they woke; `models_used` lists every model seen in a session.
- Wave `1-3` is the first set of lanes (no `W<n>` prefix); they are unrated because they were integrated in bulk.
- The grades are one reviewer's judgement of one project, and each agent got a different story, so compare models by wave and lane type, not by a single average.
