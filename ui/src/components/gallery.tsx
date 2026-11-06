import { useState } from "react";
import type { ReactNode } from "react";
import { Badge } from "./badge";
import { Button } from "./button";
import { Checkbox } from "./checkbox";
import { Chip } from "./chip";
import { Input } from "./input";
import { RadioCardGroup } from "./radio-card";
import { SegmentedControl } from "./segmented-control";
import { ITEM_STATUSES } from "./status";
import { StatusDot } from "./status-dot";
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
    </main>
  );
}
