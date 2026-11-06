import { useState } from "react";
import type { ReactNode } from "react";
import type { ColumnDef } from "@tanstack/react-table";
import { Badge } from "./badge";
import { Button } from "./button";
import { Checkbox } from "./checkbox";
import { Chip } from "./chip";
import { Input } from "./input";
import { RadioCardGroup } from "./radio-card";
import { SegmentedControl } from "./segmented-control";
import { ITEM_STATUSES } from "./status";
import { StatusDot } from "./status-dot";
import { agentColumns } from "./table/agent-columns";
import type { DataTableFeatures } from "./table/features";
import { DataTable } from "./table/table";
import type { AgentCellState, AgentDescriptor } from "./table/types";
import { Toggle } from "./toggle";

/**
 * Development gallery for the TU-002 widgets.
 *
 * It is imported dynamically from `main.tsx` only in a dev build
 * (`?gallery`), so it never ships in the release bundle. It exists to eyeball
 * every variant and to give the component tests a single page of real cases.
 */

function Section({ title, children }: { readonly title: string; readonly children: ReactNode }) {
  return (
    <section className="flex flex-col gap-12 border-b border-border pb-24">
      <h2 className="m-0 font-mono text-16 font-semibold text-text">{title}</h2>
      {children}
    </section>
  );
}

function Row({ children }: { readonly children: ReactNode }) {
  return <div className="flex flex-wrap items-center gap-8">{children}</div>;
}

const SEGMENTS = [
  { value: "global", label: "Global" },
  { value: "user", label: "User" },
  { value: "project", label: "Project" },
] as const;

type Segment = (typeof SEGMENTS)[number]["value"];

const RADIO_CARDS = [
  {
    value: "replace",
    title: "Replace the original",
    description: "Move the file into AgenTuna and leave a link in the agent folder.",
    badge: <Badge tone="accent">Recommended</Badge>,
  },
  {
    value: "watch",
    title: "Watch in place",
    description: "Keep the agent's file where it is and detect external edits.",
  },
  {
    value: "skip",
    title: "Leave unmanaged",
    description: "Scan only; the file stays outside AgenTuna.",
    disabled: true,
  },
] as const;

type Placement = (typeof RADIO_CARDS)[number]["value"];

interface GallerySkill {
  readonly id: string;
  readonly name: string;
  readonly description: string;
  readonly states: Readonly<Record<string, AgentCellState>>;
  readonly dimmed?: boolean;
  readonly tooltip?: string;
}

const GALLERY_AGENTS: readonly AgentDescriptor[] = [
  { id: "claude-code", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "omp", label: "omp" },
];

const LINKED: AgentCellState = { kind: "linked" };
const OFF: AgentCellState = { kind: "off" };
const NONE: AgentCellState = { kind: "none" };
const LOCKED: AgentCellState = { kind: "locked", lockedBy: "Global" };
const SHADOWED: AgentCellState = { kind: "shadowed", shadowedBy: "Project" };
const native = (path: string, sharedWith?: readonly string[]): AgentCellState => ({
  kind: "native",
  path,
  readOnly: true,
  sharedWith,
});

const GALLERY_SKILLS: readonly GallerySkill[] = [
  {
    id: "tdd",
    name: "tdd",
    description: "Red-green-refactor, test-first",
    states: { "claude-code": LINKED, codex: LINKED, omp: LINKED },
  },
  {
    id: "code-review",
    name: "code-review",
    description: "Review PRs against team standards",
    states: { "claude-code": LOCKED, codex: LOCKED, omp: LOCKED },
  },
  {
    id: "ui-ux-pro-max",
    name: "ui-ux-pro-max",
    description: "UI/UX design intelligence",
    states: { "claude-code": LINKED, codex: OFF, omp: LINKED },
  },
  {
    id: "pdf-extract",
    name: "pdf-extract",
    description: "Pull tables out of PDFs",
    states: { "claude-code": native(".claude/skills/pdf-extract"), codex: NONE, omp: NONE },
  },
  {
    id: "release-notes",
    name: "release-notes",
    description: "Draft release notes from git log",
    states: {
      "claude-code": native(".claude/skills/release-notes"),
      codex: native(".agents/skills/release-notes", ["omp"]),
      omp: NONE,
    },
  },
  {
    id: "review-sql",
    name: "review-sql",
    description: "Review SQL migrations",
    states: {
      "claude-code": NONE,
      codex: native("~/.agents/skills/review-sql"),
      omp: native("~/.omp/agent/skills/review-sql"),
    },
  },
  {
    id: "tdd-shadowed",
    name: "tdd",
    description: "User copy, not loaded in this repo",
    dimmed: true,
    tooltip: "Shadowed by Project · .agentuna/skills/tdd",
    states: { "claude-code": SHADOWED, codex: SHADOWED, omp: SHADOWED },
  },
];

const GALLERY_COLUMNS: ReadonlyArray<ColumnDef<DataTableFeatures, GallerySkill>> = [
  {
    accessorKey: "name",
    header: "Name",
    size: 320,
    meta: { grow: true },
    cell: ({ row }) => (
      <span className="flex min-w-0 flex-col">
        <span className="truncate font-mono text-13 text-text">{row.original.name}</span>
        <span className="truncate text-11 text-text-muted">{row.original.description}</span>
      </span>
    ),
  },
  ...agentColumns<GallerySkill>({
    agents: GALLERY_AGENTS,
    getState: (row, agent) => row.states[agent.id] ?? NONE,
  }),
];

/** Renders every widget, variant and state that ships in the library. */
export function ComponentGallery() {
  const [filter, setFilter] = useState("all");
  const [linked, setLinked] = useState(true);
  const [checked, setChecked] = useState<boolean | "indeterminate">(true);
  const [headerChecked, setHeaderChecked] = useState<boolean | "indeterminate">("indeterminate");
  const [name, setName] = useState("");
  const [segment, setSegment] = useState<Segment>("project");
  const [placement, setPlacement] = useState<Placement>("replace");

  return (
    <main className="flex min-h-full flex-col gap-24 bg-surface p-24 text-text">
      <header className="flex flex-col gap-4">
        <h1 className="m-0 font-mono text-26 font-semibold">AgenTuna · component library</h1>
        <p className="m-0 text-13 text-text-secondary">
          Every widget renders default, hover, active and focus-visible states; tab through the page to see the
          focus ring. Disabled samples follow each group.
        </p>
      </header>

      <Section title="Button">
        <Row>
          <Button variant="primary">New skill</Button>
          <Button variant="secondary">Import from git</Button>
          <Button variant="ghost">Cancel</Button>
          <Button variant="destructive">Remove</Button>
          <Button variant="primary" loading>
            Saving
          </Button>
        </Row>
        <Row>
          <Button size="sm">Small</Button>
          <Button size="md">Medium</Button>
          <Button size="lg">Large</Button>
          <Button disabled>Disabled secondary</Button>
          <Button variant="primary" disabled>
            Disabled primary
          </Button>
          <Button variant="destructive" disabled>
            Disabled destructive
          </Button>
        </Row>
      </Section>

      <Section title="Badge">
        <Row>
          {ITEM_STATUSES.map((status) => (
            <Badge key={status} status={status} />
          ))}
        </Row>
        <Row>
          <Badge tone="neutral">Not in AgenTuna</Badge>
          <Badge tone="accent">Recommended</Badge>
          <Badge tone="info" dot>
            Global
          </Badge>
          <Badge tone="muted" dot>
            User
          </Badge>
          <Badge mono tone="neutral">
            native
          </Badge>
        </Row>
      </Section>

      <Section title="Chip">
        <Row>
          <Chip pressed={filter === "all"} onPressedChange={() => setFilter("all")} count={23}>
            All
          </Chip>
          <Chip pressed={filter === "user"} onPressedChange={() => setFilter("user")} dotTone="scope-user" count={12}>
            User
          </Chip>
          <Chip
            pressed={filter === "global"}
            onPressedChange={() => setFilter("global")}
            dotTone="scope-global"
            count={5}
          >
            Global
          </Chip>
          <Chip
            pressed={filter === "project"}
            onPressedChange={() => setFilter("project")}
            dotTone="scope-project"
            count={8}
          >
            Project
          </Chip>
          <Chip
            pressed={filter === "shadowed"}
            onPressedChange={() => setFilter("shadowed")}
            dotTone="neutral"
            count={2}
          >
            Shadowed
          </Chip>
          <Chip pressed={false} onPressedChange={() => undefined} disabled count={3}>
            Disabled
          </Chip>
        </Row>
      </Section>

      <Section title="Toggle">
        <Row>
          <Toggle label="Linked" checked={linked} onCheckedChange={setLinked} />
          <Toggle label="Not linked" checked={!linked} onCheckedChange={setLinked} />
          <Toggle
            label="Background health checks"
            description="Runs only for servers you approved."
            checked={linked}
            onCheckedChange={setLinked}
          />
        </Row>
        <Row>
          <Toggle size="sm" label="Small on" checked onCheckedChange={() => undefined} />
          <Toggle size="sm" label="Small off" checked={false} onCheckedChange={() => undefined} />
          <Toggle label="Disabled on" checked onCheckedChange={() => undefined} disabled />
          <Toggle label="Disabled off" checked={false} onCheckedChange={() => undefined} disabled />
        </Row>
      </Section>

      <Section title="Checkbox">
        <Row>
          <Checkbox label="Select all" checked={checked} onCheckedChange={setChecked} />
          <Checkbox label="Indeterminate" checked="indeterminate" onCheckedChange={setChecked} />
          <Checkbox label="With description" description="Applies to every agent." checked={checked === true} onCheckedChange={setChecked} />
          <Checkbox aria-label="Select row" checked={headerChecked} onCheckedChange={setHeaderChecked} />
        </Row>
        <Row>
          <Checkbox label="Disabled unchecked" checked={false} onCheckedChange={() => undefined} disabled />
          <Checkbox label="Disabled checked" checked onCheckedChange={() => undefined} disabled />
        </Row>
      </Section>

      <Section title="Input">
        <div className="grid w-full max-w-[520px] grid-cols-1 gap-12">
          <Input label="Name" placeholder="Review pull requests" value={name} onChange={(event) => setName(event.target.value)} />
          <Input
            label="Filter skills"
            placeholder="Filter skills"
            hint="Matches name, slug and description."
            leadingIcon={
              <svg aria-hidden="true" viewBox="0 0 12 12" className="size-12">
                <circle cx="5" cy="5" r="3.25" fill="none" stroke="currentColor" strokeWidth="1.5" />
                <path d="m7.5 7.5 2.5 2.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
              </svg>
            }
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <Input
            label="Secret reference"
            error="Use ${VAR} references, never a secret value."
            mono
            defaultValue="GITHUB_PERSONAL_ACCESS_TOKEN"
          />
          <Input label="Disabled" defaultValue="Read-only value" disabled />
        </div>
      </Section>

      <Section title="Segmented control">
        <Row>
          <SegmentedControl label="Scope" value={segment} onValueChange={setSegment} options={SEGMENTS} />
          <SegmentedControl label="Disabled scope" value="user" onValueChange={() => undefined} options={SEGMENTS} disabled />
        </Row>
      </Section>

      <Section title="Radio card">
        <RadioCardGroup
          label="Migration placement"
          value={placement}
          onValueChange={setPlacement}
          options={RADIO_CARDS}
          columns={3}
          className="max-w-[900px]"
        />
      </Section>

      <Section title="Status dot">
        <Row>
          {ITEM_STATUSES.map((status) => (
            <StatusDot key={status} status={status} />
          ))}
        </Row>
      </Section>

      <Section title="Table">
        <DataTable<GallerySkill>
          ariaLabel="Skills"
          data={GALLERY_SKILLS}
          columns={GALLERY_COLUMNS}
          getRowId={(skill) => skill.id}
          getRowLabel={(skill) => skill.name}
          viewportHeight={512}
          rowTone={(skill) => (skill.dimmed === true ? "dimmed" : "default")}
          rowTooltip={(skill) => skill.tooltip}
          getColumnSpan={(skill, columnId) =>
            skill.id === "release-notes" && columnId === "agent:codex" ? 2 : 1
          }
        />
        <div className="grid grid-cols-2 gap-12">
          <DataTable<GallerySkill>
            ariaLabel="Empty skills"
            data={[]}
            columns={GALLERY_COLUMNS}
            getRowId={(skill) => skill.id}
            getRowLabel={(skill) => skill.name}
            viewportHeight={240}
            emptyTitle="No skills in this project"
            emptyBody="Import an existing skill or create one from scratch."
            emptyAction={{ label: "Add skill", onClick: () => undefined }}
          />
          <DataTable<GallerySkill>
            ariaLabel="Loading skills"
            data={[]}
            columns={GALLERY_COLUMNS}
            getRowId={(skill) => skill.id}
            getRowLabel={(skill) => skill.name}
            viewportHeight={240}
            loading
          />
        </div>
      </Section>
    </main>
  );
}
