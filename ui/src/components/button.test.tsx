import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Button, type ButtonVariant } from "./button";

afterEach(cleanup);

const VARIANTS: readonly ButtonVariant[] = ["primary", "secondary", "ghost", "destructive"];

describe("Button", () => {
  test.each(VARIANTS)("%s renders hover, active, focus and disabled state classes", (variant) => {
    render(<Button variant={variant}>Act</Button>);
    const button = screen.getByRole("button", { name: "Act" });
    const classes = button.className;

    expect(classes).toContain("hover:");
    expect(classes).toContain("active:");
    expect(classes).toContain("focus-visible:outline-focus");
    expect(classes).toContain("outline-2");
    expect(classes).toContain("outline-offset-2");
    expect(classes).toContain("disabled:");
    expect(button.dataset["variant"]).toBe(variant);
    expect(button.getAttribute("type")).toBe("button");
  });

  test("variants paint with theme tokens, not raw colours", () => {
    const { rerender } = render(<Button variant="primary">Primary</Button>);
    expect(screen.getByRole("button").className).toContain("bg-accent-fill");

    rerender(<Button variant="destructive">Destructive</Button>);
    expect(screen.getByRole("button").className).toContain("text-danger");

    rerender(<Button variant="secondary">Secondary</Button>);
    expect(screen.getByRole("button").className).toContain("border-border");

    rerender(<Button variant="ghost">Ghost</Button>);
    expect(screen.getByRole("button").className).toContain("bg-transparent");
  });

  test("sizes change the control height token", () => {
    const { rerender } = render(<Button size="sm">Act</Button>);
    expect(screen.getByRole("button").className).toContain("h-24");
    rerender(<Button size="lg">Act</Button>);
    expect(screen.getByRole("button").className).toContain("text-14");
  });

  test("click reports one interaction and never changes its own props", async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(
      <Button aria-pressed={false} onClick={onClick}>
        Save
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save" });

    await user.click(button);

    expect(onClick).toHaveBeenCalledTimes(1);
    expect(button.getAttribute("aria-pressed")).toBe("false");
  });

  test("Space and Enter activate the focused button", async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(<Button onClick={onClick}>Save</Button>);
    const button = screen.getByRole("button", { name: "Save" });

    button.focus();
    await user.keyboard(" ");
    await user.keyboard("{Enter}");

    expect(onClick).toHaveBeenCalledTimes(2);
    expect(document.activeElement).toBe(button);
  });

  test("disabled blocks activation and is removed from the tab order", async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(
      <Button disabled onClick={onClick}>
        Save
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

    await user.click(button);
    await user.tab();

    expect(onClick).not.toHaveBeenCalled();
    expect(button.disabled).toBe(true);
    expect(document.activeElement).not.toBe(button);
  });

  test("loading reports aria-busy and disables the control", async () => {
    const user = userEvent.setup();
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Saving
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Saving" }) as HTMLButtonElement;

    await user.click(button);

    expect(button.getAttribute("aria-busy")).toBe("true");
    expect(button.disabled).toBe(true);
    expect(onClick).not.toHaveBeenCalled();
  });

  test("asChild renders the child element with button styling", () => {
    render(
      <Button asChild>
        <a href="/skills">Skills</a>
      </Button>,
    );
    const link = screen.getByRole("link", { name: "Skills" });

    expect(link.tagName).toBe("A");
    expect(link.getAttribute("href")).toBe("/skills");
    expect(link.className).toContain("inline-flex");
  });
});
