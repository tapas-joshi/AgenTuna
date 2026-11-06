import { HighlightStyle } from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { EditorView } from "@codemirror/view";

/**
 * Markdown tokens rendered as class names instead of colours.
 *
 * The classes are styled by {@link codemirrorTheme} with theme variables, so a
 * theme switch re-colours the editor without rebuilding the extension. Using
 * classes also keeps the highlighting assertable in tests.
 */
export const markdownHighlightStyle = HighlightStyle.define([
  { tag: tags.heading1, class: "cm-md-heading cm-md-heading-1" },
  { tag: tags.heading2, class: "cm-md-heading cm-md-heading-2" },
  { tag: tags.heading3, class: "cm-md-heading cm-md-heading-3" },
  { tag: tags.heading4, class: "cm-md-heading cm-md-heading-4" },
  { tag: tags.heading5, class: "cm-md-heading cm-md-heading-5" },
  { tag: tags.heading6, class: "cm-md-heading cm-md-heading-6" },
  { tag: tags.strong, class: "cm-md-strong" },
  { tag: tags.emphasis, class: "cm-md-emphasis" },
  { tag: tags.link, class: "cm-md-link" },
  { tag: tags.url, class: "cm-md-url" },
  { tag: tags.monospace, class: "cm-md-code" },
  { tag: tags.quote, class: "cm-md-quote" },
  { tag: tags.list, class: "cm-md-list" },
  { tag: tags.contentSeparator, class: "cm-md-rule" },
  { tag: tags.processingInstruction, class: "cm-md-mark" },
]);

/**
 * CodeMirror chrome styled only through the theme token table.
 *
 * Everything is `var(--at-…)`, so dark and light modes restyle the editor
 * through the same `data-theme` switch as the rest of the app. Line height and
 * font size are fixed here rather than inherited so gutter and content rows
 * stay in lockstep.
 */
export const codemirrorTheme = EditorView.theme({
  "&": {
    height: "100%",
    color: "var(--at-color-text)",
    backgroundColor: "var(--at-color-surface)",
  },
  "&.cm-focused": {
    outline: "none",
  },
  ".cm-scroller": {
    fontFamily: "var(--at-font-mono)",
    fontSize: "var(--at-text-13)",
    lineHeight: "1.5",
    overflow: "auto",
  },
  ".cm-content": {
    padding: "var(--at-space-8) 0",
    caretColor: "var(--at-color-accent)",
  },
  ".cm-line": {
    padding: "0 var(--at-space-12)",
  },
  ".cm-gutters": {
    backgroundColor: "var(--at-color-surface)",
    color: "var(--at-color-text-muted)",
    border: "none",
    borderRight: "1px solid var(--at-color-border)",
  },
  ".cm-lineNumbers .cm-gutterElement": {
    padding: "0 var(--at-space-8) 0 var(--at-space-12)",
  },
  ".cm-activeLine": {
    backgroundColor: "var(--at-color-overlay)",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "var(--at-color-overlay)",
    color: "var(--at-color-text)",
  },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
    backgroundColor: "color-mix(in srgb, var(--at-color-accent) 25%, transparent)",
  },
  ".cm-cursor, .cm-dropCursor": {
    borderLeftColor: "var(--at-color-accent)",
  },
  ".cm-md-heading": {
    color: "var(--at-color-text)",
    fontWeight: "600",
  },
  ".cm-md-strong": {
    color: "var(--at-color-text)",
    fontWeight: "600",
  },
  ".cm-md-emphasis": {
    fontStyle: "italic",
  },
  ".cm-md-link": {
    color: "var(--at-color-accent)",
  },
  ".cm-md-url": {
    color: "var(--at-color-text-muted)",
  },
  ".cm-md-code": {
    color: "var(--at-color-warning)",
  },
  ".cm-md-quote": {
    color: "var(--at-color-text-secondary)",
    fontStyle: "italic",
  },
  ".cm-md-list": {
    color: "var(--at-color-text-secondary)",
  },
  ".cm-md-rule": {
    color: "var(--at-color-border)",
  },
  ".cm-md-mark": {
    color: "var(--at-color-text-muted)",
  },
});
