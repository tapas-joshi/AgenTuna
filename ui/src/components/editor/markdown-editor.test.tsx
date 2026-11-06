import "./codemirror-test-setup";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef, useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";
import { MarkdownEditor, hasMalformedUtf16 } from "./markdown-editor";
import type { MarkdownEditorHandle } from "./markdown-editor";

afterEach(cleanup);

interface HarnessProps {
  readonly editorRef: React.Ref<MarkdownEditorHandle>;
  readonly initial?: string;
  readonly baseline?: string;
  readonly error?: string | null;
  readonly onSave?: (value: string) => void;
  readonly onChange?: (value: string) => void;
}

function Harness({ editorRef, initial = "# Title\n\nBody", baseline, error, onSave, onChange }: HarnessProps) {
  const [value, setValue] = useState(initial);
  return (
    <MarkdownEditor
      ref={editorRef}
      label="SKILL.md"
      value={value}
      baseline={baseline ?? initial}
      error={error}
      onChange={(next) => {
        setValue(next);
        onChange?.(next);
      }}
      onSave={onSave}
    />
  );
}

async function waitForEditor(container: HTMLElement): Promise<void> {
  await waitFor(() => expect(container.querySelector(".cm-editor")).not.toBeNull());
}

describe("MarkdownEditor mount", () => {
  test("renders the accessible textbox, line numbers and active-line highlight", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    const { container } = render(<Harness editorRef={ref} />);
    await waitForEditor(container);

    expect(screen.getByRole("textbox", { name: "SKILL.md" })).toBeTruthy();
    expect(container.querySelector(".cm-lineNumbers")).not.toBeNull();
    expect(container.querySelector(".cm-activeLine")).not.toBeNull();
    expect(ref.current?.getValue()).toBe("# Title\n\nBody");
  });

  test("highlights markdown headings through theme classes", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    const { container } = render(<Harness editorRef={ref} />);
    await waitForEditor(container);

    await waitFor(() => expect(container.querySelector(".cm-md-heading")).not.toBeNull());
  });

  test("mounts a 2,000-line file with line numbers and both view modes available", async () => {
    const value = ["# Heading", ...Array.from({ length: 2_000 }, (_, index) => `line ${index + 1}`)].join(
      "\n",
    );
    const ref = createRef<MarkdownEditorHandle>();
    const { container } = render(<Harness editorRef={ref} initial={value} />);
    await waitForEditor(container);

    expect(container.querySelector(".cm-lineNumbers")?.textContent).toContain("1");
    expect(container.querySelector(".cm-activeLine")).not.toBeNull();
    await waitFor(() => expect(container.querySelector(".cm-md-heading")).not.toBeNull());
    expect(screen.getByRole("radio", { name: "Edit" })).toBeTruthy();
    expect(screen.getByRole("radio", { name: "Preview" })).toBeTruthy();
    expect(ref.current?.getValue()).toBe(value);
  });
});

describe("Edit/Preview", () => {
  test("switches to a sanitized markdown preview and back", async () => {
    const user = userEvent.setup();
    const ref = createRef<MarkdownEditorHandle>();
    const { container } = render(
      <Harness editorRef={ref} initial={"# Title\n\n**bold** text"} />,
    );
    await waitForEditor(container);

    await user.click(screen.getByRole("radio", { name: "Preview" }));

    expect(screen.getByRole("heading", { level: 1, name: "Title" })).toBeTruthy();
    expect(container.querySelector('[data-slot="markdown-preview"]')?.textContent).toContain("bold");
    const host = container.querySelector('[data-slot="markdown-editor"]');
    expect(host?.closest(".hidden")).not.toBeNull();

    await user.click(screen.getByRole("radio", { name: "Edit" }));
    expect(host?.closest(".hidden")).toBeNull();
  });

  test("never renders raw HTML, script, images or javascript: links", async () => {
    const user = userEvent.setup();
    const ref = createRef<MarkdownEditorHandle>();
    const source = [
      "# Title",
      "",
      "<script>window.hacked = 1</script>",
      '<img src="x" onerror="window.hacked = 2">',
      "",
      "[bad](javascript:alert(1))",
      "",
      "![remote](https://example.com/a.png)",
    ].join("\n");
    const { container } = render(<Harness editorRef={ref} initial={source} />);
    await waitForEditor(container);

    await user.click(screen.getByRole("radio", { name: "Preview" }));
    const preview = container.querySelector('[data-slot="markdown-preview"]');

    expect(preview?.querySelector("script, iframe, img, object")).toBeNull();
    expect(preview?.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(preview?.textContent).toContain("[image: remote]");
    expect((window as unknown as Record<string, unknown>)["hacked"]).toBeUndefined();
  });
});

describe("dirty tracking and save", () => {
  test("shows the dirty dot and Cmd+S hands the current buffer to onSave", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    const onSave = vi.fn();
    const onChange = vi.fn();
    render(<Harness editorRef={ref} initial="hello" onSave={onSave} onChange={onChange} />);
    await waitForEditor(document.body);

    expect(screen.queryByLabelText("Unsaved changes")).toBeNull();

    act(() => {
      ref.current?.setCursor(ref.current.getValue().length);
      ref.current?.insertText(" world");
    });

    await waitFor(() => expect(screen.getByLabelText("Unsaved changes")).toBeTruthy());
    expect(onChange).toHaveBeenLastCalledWith("hello world");

    ref.current?.focus();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "s", metaKey: true });

    expect(onSave).toHaveBeenCalledWith("hello world");
  });

  test("hides the dirty dot when the buffer matches the baseline again", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    render(<Harness editorRef={ref} initial="hello" />);
    await waitForEditor(document.body);

    act(() => ref.current?.insertText("!"));
    await waitFor(() => expect(screen.getByLabelText("Unsaved changes")).toBeTruthy());

    act(() => ref.current?.undo());
    await waitFor(() => expect(screen.queryByLabelText("Unsaved changes")).toBeNull());
  });
});

describe("undo and redo", () => {
  test("covers programmatic edits and responds to Cmd+Z / Cmd+Shift+Z", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    render(<Harness editorRef={ref} initial="hello" />);
    await waitForEditor(document.body);

    act(() => {
      ref.current?.setCursor(ref.current.getValue().length);
      ref.current?.insertText(" one");
      ref.current?.insertText(" two");
    });
    expect(ref.current?.getValue()).toBe("hello one two");

    ref.current?.focus();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "z", metaKey: true });
    expect(ref.current?.getValue()).toBe("hello one");

    fireEvent.keyDown(document.activeElement ?? document.body, {
      key: "Z",
      keyCode: 90,
      metaKey: true,
      shiftKey: true,
    });
    expect(ref.current?.getValue()).toBe("hello one two");
  });

  test("a new edit after undo discards the redo branch", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    render(<Harness editorRef={ref} initial="a" />);
    await waitForEditor(document.body);

    act(() => {
      ref.current?.setCursor(1);
      ref.current?.insertText("b");
    });
    act(() => ref.current?.undo());
    expect(ref.current?.getValue()).toBe("a");

    act(() => ref.current?.insertText("c"));
    act(() => ref.current?.redo());
    expect(ref.current?.getValue()).toBe("ca");
  });
});

describe("unreadable input", () => {
  test("freezes the previous buffer, shows an error and stays read-only", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    const onChange = vi.fn();
    const { rerender } = render(
      <MarkdownEditor ref={ref} label="SKILL.md" value="hello" baseline="hello" onChange={onChange} />,
    );
    await waitForEditor(document.body);
    expect(ref.current?.getValue()).toBe("hello");

    act(() => {
      ref.current?.setCursor(ref.current.getValue().length);
      ref.current?.insertText(" world");
    });
    expect(ref.current?.getValue()).toBe("hello world");
    expect(onChange).toHaveBeenCalledTimes(1);

    rerender(
      <MarkdownEditor
        ref={ref}
        label="SKILL.md"
        value={"\uD800"}
        baseline="hello"
        onChange={onChange}
      />,
    );

    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("malformed UTF-8"));
    expect(ref.current?.getValue()).toBe("hello world");
    expect(screen.getByRole("textbox", { name: "SKILL.md" }).getAttribute("aria-readonly")).toBe("true");

    act(() => ref.current?.insertText("x"));
    act(() => ref.current?.undo());
    expect(ref.current?.getValue()).toBe("hello world");
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  test("an explicit read error keeps the loaded text visible but read-only", async () => {
    const ref = createRef<MarkdownEditorHandle>();
    render(
      <MarkdownEditor
        ref={ref}
        label="SKILL.md"
        value="loaded text"
        baseline="loaded text"
        error="Permission denied"
        onChange={() => undefined}
      />,
    );
    await waitForEditor(document.body);

    expect(screen.getByRole("alert").textContent).toBe("Permission denied");
    expect(ref.current?.getValue()).toBe("loaded text");
    act(() => ref.current?.insertText("x"));
    expect(ref.current?.getValue()).toBe("loaded text");
  });

  test("hasMalformedUtf16 accepts well-formed surrogate pairs", () => {
    expect(hasMalformedUtf16("plain")).toBe(false);
    expect(hasMalformedUtf16("emoji 😀")).toBe(false);
    expect(hasMalformedUtf16("\uD800")).toBe(true);
    expect(hasMalformedUtf16("a\uDC00b")).toBe(true);
  });
});

describe("wrapping", () => {
  test("Wrap toggles the CodeMirror line-wrapping extension", async () => {
    const user = userEvent.setup();
    const ref = createRef<MarkdownEditorHandle>();
    const { container } = render(<Harness editorRef={ref} />);
    await waitForEditor(container);

    const content = container.querySelector(".cm-content");
    expect(content?.classList.contains("cm-lineWrapping")).toBe(true);

    await user.click(screen.getByRole("switch", { name: "Wrap" }));
    await waitFor(() =>
      expect(container.querySelector(".cm-content")?.classList.contains("cm-lineWrapping")).toBe(false),
    );

    await user.click(screen.getByRole("switch", { name: "Wrap" }));
    await waitFor(() =>
      expect(container.querySelector(".cm-content")?.classList.contains("cm-lineWrapping")).toBe(true),
    );
  });

  test("long lines keep the caret in the document when wrapping is off", async () => {
    const user = userEvent.setup();
    const ref = createRef<MarkdownEditorHandle>();
    render(<Harness editorRef={ref} initial="short" />);
    await waitForEditor(document.body);

    await user.click(screen.getByRole("switch", { name: "Wrap" }));
    act(() => {
      ref.current?.setCursor(ref.current.getValue().length);
      ref.current?.insertText(" ".repeat(200) + "end");
    });

    expect(ref.current?.getValue().endsWith("end")).toBe(true);
    expect(screen.getByRole("textbox", { name: "SKILL.md" })).toBeTruthy();
  });
});
