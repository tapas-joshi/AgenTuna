import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { Input } from "./input";

afterEach(cleanup);

function ControlledField({ onValue }: { readonly onValue: (value: string) => void }) {
  const [value, setValue] = useState("");
  return (
    <Input
      label="Name"
      placeholder="Review pull requests"
      value={value}
      onChange={(event) => {
        setValue(event.target.value);
        onValue(event.target.value);
      }}
    />
  );
}

describe("Input", () => {
  test("is associated with its visible label and accepts typing", async () => {
    const user = userEvent.setup();
    const onValue = vi.fn();
    render(<ControlledField onValue={onValue} />);
    const input = screen.getByLabelText("Name");

    expect(input.tagName).toBe("INPUT");
    await user.type(input, "triage");

    expect(onValue).toHaveBeenLastCalledWith("triage");
    expect((input as HTMLInputElement).value).toBe("triage");
  });

  test("focus draws the ring on the whole field box", async () => {
    const user = userEvent.setup();
    render(<Input label="Name" defaultValue="" />);
    const input = screen.getByLabelText("Name");
    const box = input.parentElement;

    expect(box?.className).toContain("has-[:focus-visible]:outline-focus");
    expect(box?.className).toContain("has-[:focus-visible]:outline-offset-2");

    await user.tab();
    expect(document.activeElement).toBe(input);
  });

  test("hint text is linked through aria-describedby", () => {
    render(<Input label="Filter" hint="Matches name, slug and description." />);
    const input = screen.getByLabelText("Filter");
    const describedBy = input.getAttribute("aria-describedby");

    expect(document.getElementById(describedBy ?? "")?.textContent).toBe(
      "Matches name, slug and description.",
    );
    // Without an error the field is valid and has no danger border.
    expect(input.getAttribute("aria-invalid")).toBeNull();
    expect(input.parentElement?.className).toContain("border-border");
  });

  test("an error sets aria-invalid, links the message and switches the border token", () => {
    render(<Input label="Path" error="Use a ${VAR} reference, never a secret value." />);
    const input = screen.getByLabelText("Path");
    const describedBy = input.getAttribute("aria-describedby");
    const message = document.getElementById(describedBy ?? "");

    expect(input.getAttribute("aria-invalid")).toBe("true");
    expect(message?.textContent).toBe("Use a ${VAR} reference, never a secret value.");
    expect(input.parentElement?.className).toContain("border-danger");
  });

  test("mono renders identifiers in the mono face", () => {
    render(<Input label="Secret reference" mono defaultValue="GITHUB_PERSONAL_ACCESS_TOKEN" />);
    expect(screen.getByLabelText("Secret reference").className).toContain("font-mono");
  });

  test("an aria-label-only field still has an accessible name", () => {
    render(<Input aria-label="Search skills" placeholder="Search" />);
    expect(screen.getByRole("textbox", { name: "Search skills" })).toBeTruthy();
  });

  test("a leading icon is decorative", () => {
    render(
      <Input
        label="Filter"
        leadingIcon={<svg aria-hidden="true" viewBox="0 0 12 12" />}
      />,
    );
    const input = screen.getByLabelText("Filter");
    expect(input.parentElement?.querySelector("[aria-hidden='true']")).not.toBeNull();
  });

  test("disabled inputs are skipped by Tab and cannot be edited", async () => {
    const user = userEvent.setup();
    render(<Input label="Read only" defaultValue="fixed" disabled />);
    const input = screen.getByLabelText("Read only") as HTMLInputElement;

    await user.type(input, "x");
    await user.tab();

    expect(input.disabled).toBe(true);
    expect(input.value).toBe("fixed");
    expect(document.activeElement).not.toBe(input);
  });
});
