import { cleanup, render } from "@testing-library/react";
import { axe } from "jest-axe";
import { afterEach, expect, test, vi } from "vitest";
import { ComponentGallery } from "./components/gallery";
import { Shell } from "./shell/Shell";

afterEach(cleanup);


const shellProps = {
  route: "overview" as const,
  title: "Overview",
  onNavigate: vi.fn(),
  onProjectPicker: vi.fn(),
  onPalette: vi.fn(),
  children: <h1>Overview</h1>,
};

test("app shell has no axe accessibility violations", async () => {
  const { container } = render(<Shell {...shellProps} />);
  expect((await axe(container)).violations).toEqual([]);
});

test("shared component gallery has no axe accessibility violations", async () => {
  const { container } = render(<ComponentGallery />);
  expect((await axe(container)).violations).toEqual([]);
});
