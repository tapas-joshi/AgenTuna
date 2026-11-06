import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";
import { applyDensity } from "../../fonts";
import {
  SCREEN_STATES,
  STATE_CHROME_IDS,
  STATE_ROUTE_IDS,
  STATE_SURFACE_IDS,
  SURFACE_STATE_FIXTURES,
  validateSurfaceFixtures,
  type SurfaceStateFixture,
} from "./fixtures";
import { StateView } from "./state-view";
import { STATE_VARIANTS, type StateVariant } from "./types";

afterEach(cleanup);

/** The route ids TU-010 defines, spelled out so a rename in the router fails here. */
const TU_010_ROUTE_IDS = [
  "overview",
  "skills",
  "skill-detail",
  "mcp-list",
  "mcp-detail",
  "mcp-form",
  "effective",
  "drift",
  "instructions",
  "agents",
  "add-skill",
  "migrate",
  "settings",
];

describe("state fixture matrix", () => {
  test("covers every TU-010 route id exactly once", () => {
    expect([...STATE_ROUTE_IDS]).toEqual(TU_010_ROUTE_IDS);
    expect(new Set(STATE_ROUTE_IDS).size).toBe(TU_010_ROUTE_IDS.length);
  });

  test("adds the shell and overlay surfaces", () => {
    expect([...STATE_CHROME_IDS]).toEqual(["shell", "overlay"]);
    expect([...STATE_SURFACE_IDS]).toEqual([...TU_010_ROUTE_IDS, "shell", "overlay"]);
  });

  test("every surface has all four states plus the offline copy", () => {
    expect(validateSurfaceFixtures(SURFACE_STATE_FIXTURES)).toEqual([]);
    for (const id of STATE_SURFACE_IDS) {
      const fixture = SURFACE_STATE_FIXTURES[id];
      expect(Object.keys(fixture).sort()).toEqual(["empty", "error", "loading", "offline", "populated"]);
    }
  });

  test("the validator fails on a missing combination", () => {
    const broken = {
      ...SURFACE_STATE_FIXTURES,
      skills: { ...SURFACE_STATE_FIXTURES.skills, error: undefined },
    } as unknown as Readonly<Record<(typeof STATE_SURFACE_IDS)[number], SurfaceStateFixture>>;

    expect(validateSurfaceFixtures(broken)).toEqual(["skills: missing error"]);
  });

  test("every route and surface renders its empty, loading, error and offline fixture", () => {
    for (const id of STATE_SURFACE_IDS) {
      const fixture = SURFACE_STATE_FIXTURES[id];
      for (const state of ["empty", "loading", "error", "offline"] as const) {
        const view = fixture[state];
        render(<StateView variant={view.variant} title={view.title} body={view.body} details={view.details} />);
        expect(screen.getByRole("heading", { name: view.title })).toBeTruthy();
        if (state === "error") {
          expect(screen.getByRole("button", { name: /Technical details/ })).toBeTruthy();
        }
        cleanup();
      }
    }
  });

  test("all four variants are exercised and the populated fixtures have rows", () => {
    const variants = new Set<StateVariant>();
    for (const id of STATE_SURFACE_IDS) {
      const fixture = SURFACE_STATE_FIXTURES[id];
      for (const state of ["empty", "loading", "error", "offline"] as const) {
        variants.add(fixture[state].variant);
      }
      expect(fixture.populated.rows).toBeGreaterThan(0);
      expect(fixture.populated.note.length).toBeGreaterThan(0);
    }
    expect([...variants].sort()).toEqual([...STATE_VARIANTS].sort());
  });

  test.each(SCREEN_STATES)("the %s state exists for every surface", (state) => {
    for (const id of STATE_SURFACE_IDS) {
      expect(SURFACE_STATE_FIXTURES[id][state]).toBeDefined();
    }
  });

  test.each(["1x", "2x"] as const)("renders headlessly at %s density", (bucket) => {
    applyDensity(document.documentElement, bucket === "1x" ? 1 : 2);
    expect(document.documentElement.getAttribute("data-density")).toBe(bucket);

    for (const id of STATE_SURFACE_IDS) {
      const view = SURFACE_STATE_FIXTURES[id].empty;
      render(<StateView variant={view.variant} title={view.title} body={view.body} />);
      expect(screen.getByRole("heading", { name: view.title })).toBeTruthy();
      cleanup();
    }
  });
});
