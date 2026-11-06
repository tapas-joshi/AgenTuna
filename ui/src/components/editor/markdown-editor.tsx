import { defaultKeymap } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { syntaxHighlighting } from "@codemirror/language";
import { Annotation, Compartment, EditorState } from "@codemirror/state";
import type { Extension } from "@codemirror/state";
import {
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { cn } from "../cn";
import { SegmentedControl } from "../segmented-control";
import { Toggle } from "../toggle";
import { boundedHistoryExtension, redoHistory, undoHistory } from "./bounded-history";
import { codemirrorTheme, markdownHighlightStyle } from "./cm-theme";
import { MarkdownPreview } from "./markdown-preview";

/** Which pane the editor shows. */
export type MarkdownEditorMode = "edit" | "preview";

/** Imperative operations for screens that need to drive the buffer. */
export interface MarkdownEditorHandle {
  focus(): void;
  /** Moves the caret to `anchor` (clamped to the document) and focuses the editor. */
  setCursor(anchor: number): void;
  /** Inserts `text` at the cursor as one undoable programmatic edit. */
  insertText(text: string): void;
  undo(): void;
  redo(): void;
  getValue(): string;
}

/** Props accepted by {@link MarkdownEditor}. */
export interface MarkdownEditorProps {
  /** Accessible name and visible tab title. */
  readonly label: string;
  /** Current buffer; the caller owns it. */
  readonly value: string;
  /** Text considered saved, used for the dirty dot. */
  readonly baseline: string;
  /** Reports every user or programmatic edit with the whole buffer. */
  readonly onChange: (value: string) => void;
  /** Cmd+S / Ctrl+S callback with the current buffer. */
  readonly onSave?: (value: string) => void | Promise<void>;
  /** Disables editing without showing an error. */
  readonly readOnly?: boolean;
  /** Unreadable file error; keeps the previous buffer and forces read-only. */
  readonly error?: string | null;
  /** Controlled pane; omit to let the editor keep it. */
  readonly mode?: MarkdownEditorMode;
  readonly onModeChange?: (mode: MarkdownEditorMode) => void;
  /** Controlled line wrapping; omit to let the editor keep it. */
  readonly wrap?: boolean;
  readonly onWrapChange?: (wrap: boolean) => void;
  /** Replaces the default half-viewport height; size the editor here when embedding. */
  readonly className?: string;
}

const VIEW_OPTIONS = [
  { value: "edit", label: "Edit" },
  { value: "preview", label: "Preview" },
] as const;

const MALFORMED_UTF8 = "The file contains malformed UTF-8, so it cannot be edited.";

const externalSync = Annotation.define<boolean>();

/**
 * True when `text` contains a lone UTF-16 surrogate.
 *
 * A JavaScript string cannot carry arbitrary bytes, so WebView text decoded
 * from a file with a decoding error arrives as replacement characters or as
 * lone surrogates when a caller hand-built it. Either value indicates the
 * buffer is not editable as UTF-8 text.
 */
export function hasMalformedUtf16(text: string): boolean {
  for (let index = 0; index < text.length; index += 1) {
    const code = text.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = text.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        return true;
      }
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return true;
    }
  }
  return false;
}

function contentAttributes(label: string, readOnly: boolean): Extension {
  return EditorView.contentAttributes.of({
    "aria-label": label,
    "aria-multiline": "true",
    "aria-readonly": readOnly ? "true" : "false",
  });
}

/**
 * Markdown/code editor with line numbers, active-line highlight, markdown
 * highlighting, an Edit/Preview switch, dirty tracking and a Cmd+S save hook.
 *
 * The component never writes files: it reports the buffer through `onChange`
 * and hands the current buffer to `onSave`. A malformed buffer is frozen
 * read-only so a decode failure cannot destroy the last good text.
 */
export const MarkdownEditor = forwardRef<MarkdownEditorHandle, MarkdownEditorProps>(
  function MarkdownEditor(
    {
      label,
      value,
      baseline,
      onChange,
      onSave,
      readOnly: controlledReadOnly = false,
      error = null,
      mode: controlledMode,
      onModeChange,
      wrap: controlledWrap,
      onWrapChange,
      className,
    }: MarkdownEditorProps,
    ref,
  ) {
    const hostRef = useRef<HTMLDivElement | null>(null);
    const viewRef = useRef<EditorView | null>(null);
    const readOnlyCompartment = useRef(new Compartment());
    const wrapCompartment = useRef(new Compartment());
    const attributesCompartment = useRef(new Compartment());

    const [internalMode, setInternalMode] = useState<MarkdownEditorMode>("edit");
    const [internalWrap, setInternalWrap] = useState(true);
    const mode = controlledMode ?? internalMode;
    const wrap = controlledWrap ?? internalWrap;

    const malformed = useMemo(() => hasMalformedUtf16(value), [value]);
    const errorState = error?.trim() || (malformed ? MALFORMED_UTF8 : null);
    const readable = errorState === null;
    const [frozenValue, setFrozenValue] = useState(() => (hasMalformedUtf16(value) ? "" : value));
    const displayedValue = readable ? value : frozenValue;
    const readOnly = controlledReadOnly || errorState !== null;

    const onChangeRef = useRef(onChange);
    const onSaveRef = useRef(onSave);
    const readOnlyRef = useRef(readOnly);
    const initialValueRef = useRef(displayedValue);
    onChangeRef.current = onChange;
    onSaveRef.current = onSave;
    readOnlyRef.current = readOnly;

    useEffect(() => {
      if (readable) {
        setFrozenValue(value);
      }
    }, [readable, value]);

    useEffect(() => {
      const parent = hostRef.current;
      if (parent === null) {
        return;
      }
      const readOnlyExtensions = readOnlyRef.current
        ? [EditorState.readOnly.of(true), EditorView.editable.of(false)]
        : [];
      const runSave = (view: EditorView) => {
        if (!readOnlyRef.current) {
          void onSaveRef.current?.(view.state.doc.toString());
        }
        return true;
      };
      const state = EditorState.create({
        doc: initialValueRef.current,
        extensions: [
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightActiveLine(),
          drawSelection(),
          dropCursor(),
          markdown(),
          syntaxHighlighting(markdownHighlightStyle),
          codemirrorTheme,
          boundedHistoryExtension(),
          keymap.of([
            { key: "Mod-s", run: runSave },
            { key: "Meta-s", run: runSave },
            { key: "Ctrl-s", run: runSave },
          ]),
          keymap.of(defaultKeymap),
          attributesCompartment.current.of(contentAttributes(label, readOnlyRef.current)),
          wrapCompartment.current.of(wrap ? EditorView.lineWrapping : []),
          readOnlyCompartment.current.of(readOnlyExtensions),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged) {
              return;
            }
            const external = update.transactions.some(
              (transaction) => transaction.annotation(externalSync) === true,
            );
            if (!external) {
              onChangeRef.current(update.state.doc.toString());
            }
          }),
        ],
      });
      const view = new EditorView({ state, parent });
      viewRef.current = view;
      return () => {
        view.destroy();
        viewRef.current = null;
      };
    }, []);

    useEffect(() => {
      const view = viewRef.current;
      if (view === null) {
        return;
      }
      const extensions = readOnly
        ? [EditorState.readOnly.of(true), EditorView.editable.of(false)]
        : [];
      view.dispatch({ effects: readOnlyCompartment.current.reconfigure(extensions) });
    }, [readOnly]);

    useEffect(() => {
      const view = viewRef.current;
      if (view === null) {
        return;
      }
      view.dispatch({
        effects: wrapCompartment.current.reconfigure(wrap ? EditorView.lineWrapping : []),
      });
    }, [wrap]);

    useEffect(() => {
      const view = viewRef.current;
      if (view === null) {
        return;
      }
      view.dispatch({
        effects: attributesCompartment.current.reconfigure(contentAttributes(label, readOnly)),
      });
    }, [label, readOnly]);

    useEffect(() => {
      const view = viewRef.current;
      if (view === null || !readable) {
        return;
      }
      if (view.state.doc.toString() === value) {
        return;
      }
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: value },
        annotations: externalSync.of(true),
        scrollIntoView: true,
      });
    }, [readable, value]);

    useEffect(() => {
      if (mode === "edit") {
        viewRef.current?.requestMeasure();
      }
    }, [mode]);

    useImperativeHandle(
      ref,
      () => ({
        focus() {
          viewRef.current?.focus();
        },
        setCursor(anchor: number) {
          const view = viewRef.current;
          if (view === null) {
            return;
          }
          const position = Math.max(0, Math.min(anchor, view.state.doc.length));
          view.dispatch({ selection: { anchor: position }, scrollIntoView: true });
          view.focus();
        },
        insertText(text: string) {
          const view = viewRef.current;
          if (view === null || readOnlyRef.current) {
            return;
          }
          const { from, to } = view.state.selection.main;
          view.dispatch({
            changes: { from, to, insert: text },
            selection: { anchor: from + text.length },
            scrollIntoView: true,
          });
        },
        undo() {
          const view = viewRef.current;
          if (view !== null) {
            undoHistory(view);
          }
        },
        redo() {
          const view = viewRef.current;
          if (view !== null) {
            redoHistory(view);
          }
        },
        getValue() {
          return viewRef.current?.state.doc.toString() ?? displayedValue;
        },
      }),
      [displayedValue],
    );

    const changeMode = (next: MarkdownEditorMode) => {
      if (controlledMode === undefined) {
        setInternalMode(next);
      }
      onModeChange?.(next);
    };

    const changeWrap = (next: boolean) => {
      if (controlledWrap === undefined) {
        setInternalWrap(next);
      }
      onWrapChange?.(next);
    };

    const dirty = displayedValue !== baseline;

    return (
      <div
        data-slot="markdown-editor-root"
        data-dirty={dirty ? "true" : undefined}
        className={cn(
          "flex min-h-0 flex-col overflow-hidden rounded-6 border border-border bg-surface",
          "focus-within:outline-2 focus-within:outline-offset-2 focus-within:outline-focus",
          className ?? "h-[50vh]",
        )}
      >
        <div className="flex min-w-0 flex-wrap items-center gap-8 border-b border-border bg-raised px-8 py-4">
          <span
            data-slot="editor-tab"
            data-dirty={dirty ? "true" : undefined}
            className="flex min-w-0 items-center gap-4 text-13 font-medium text-text"
          >
            <span className="truncate" title={label}>
              {label}
            </span>
            {dirty ? (
              <span
                data-slot="dirty-dot"
                role="img"
                aria-label="Unsaved changes"
                title="Unsaved changes"
                className="size-8 shrink-0 rounded-full bg-warning"
              />
            ) : null}
          </span>
          <div className="ml-auto flex min-w-0 flex-wrap items-center gap-8">
            {mode === "edit" ? (
              <Toggle size="sm" label="Wrap" checked={wrap} onCheckedChange={changeWrap} />
            ) : null}
            <SegmentedControl<MarkdownEditorMode>
              label="Editor view"
              value={mode}
              onValueChange={changeMode}
              options={VIEW_OPTIONS}
            />
          </div>
        </div>
        {errorState === null ? null : (
          <p
            role="alert"
            data-slot="editor-error"
            className="border-b border-border bg-raised px-8 py-4 text-12 text-danger"
          >
            {errorState}
          </p>
        )}
        <div className={mode === "edit" ? "min-h-0 flex-1" : "hidden"}>
          <div ref={hostRef} data-slot="markdown-editor" className="h-full" />
        </div>
        {mode === "preview" ? <MarkdownPreview markdown={displayedValue} className="flex-1" /> : null}
      </div>
    );
  },
);
