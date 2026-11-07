import type { SyntheticEvent } from "react";
import ReactMarkdown from "react-markdown";
import type { Components } from "react-markdown";
import rehypeSanitize from "rehype-sanitize";
import { cn } from "../cn";
import { isSafeExternalUrl, openExternalLink } from "./external-link";

/** Props accepted by {@link MarkdownPreview}. */
export interface MarkdownPreviewProps {
  /** Markdown source. Treated as untrusted text. */
  readonly markdown: string;
  readonly className?: string;
}

/**
 * Routes an activated link through the browser command instead of the webview.
 *
 * Runs for the mouse click and for the Enter key on a focused link; cancelling both means the
 * webview never starts a navigation, so the app-origin guard in `at-app` is the second line of
 * defence rather than the only one.
 */
function activateExternalLink(event: SyntheticEvent, url: string): void {
  event.preventDefault();
  void openExternalLink(url).catch((error: unknown) => {
    console.error("the external link could not be opened", error);
  });
}

/**
 * Markdown is never rendered as raw HTML: `skipHtml` drops HTML nodes before
 * React sees them, and `rehype-sanitize` re-runs the GitHub schema over whatever
 * the parser produced. Images are rendered as text so previewing an untrusted
 * skill cannot fetch remote content. Links never navigate the webview: an
 * `https` target is intercepted on click and on Enter and handed to the typed
 * `open_external_link` command, which opens the system browser after validating
 * the URL again; every other target renders as plain text.
 */
const COMPONENTS: Components = {
  h1: ({ children }) => <h1 className="mt-16 mb-8 text-18 font-semibold text-text first:mt-0">{children}</h1>,
  h2: ({ children }) => <h2 className="mt-16 mb-8 text-15 font-semibold text-text first:mt-0">{children}</h2>,
  h3: ({ children }) => <h3 className="mt-12 mb-4 text-14 font-semibold text-text first:mt-0">{children}</h3>,
  h4: ({ children }) => <h4 className="mt-12 mb-4 text-13 font-semibold text-text first:mt-0">{children}</h4>,
  p: ({ children }) => <p className="my-8 text-13 text-text">{children}</p>,
  ul: ({ children }) => <ul className="my-8 flex list-disc flex-col gap-2 pl-24 text-13 text-text">{children}</ul>,
  ol: ({ children }) => <ol className="my-8 flex list-decimal flex-col gap-2 pl-24 text-13 text-text">{children}</ol>,
  li: ({ children }) => <li className="pl-4">{children}</li>,
  code: ({ children }) => (
    <code className="rounded-3 bg-overlay px-4 font-mono text-12 text-warning">{children}</code>
  ),
  pre: ({ children }) => (
    <pre className="my-12 overflow-auto rounded-6 border border-border bg-overlay p-12 text-12 [&>code]:bg-transparent [&>code]:p-0 [&>code]:text-text">
      {children}
    </pre>
  ),
  blockquote: ({ children }) => (
    <blockquote className="my-12 border-l-2 border-border pl-12 text-text-secondary">{children}</blockquote>
  ),
  a: ({ children, href }) =>
    isSafeExternalUrl(href) ? (
      <a
        href={href}
        title={href}
        className="text-accent underline underline-offset-2"
        onClick={(event) => activateExternalLink(event, href)}
        onKeyDown={(event) => {
          if (event.key === "Enter") activateExternalLink(event, href);
        }}
      >
        {children}
      </a>
    ) : (
      <span className="text-text" title={href ?? undefined}>
        {children}
      </span>
    ),
  hr: () => <hr className="my-16 border-border" />,
  img: ({ alt }) => <span className="text-text-muted">[image: {alt ?? "untitled"}]</span>,
};

/**
 * Renders sanitized Markdown for the editor preview and instruction screens.
 *
 * The renderer owns no state; it displays the `markdown` it is given.
 */
export function MarkdownPreview({ markdown, className }: MarkdownPreviewProps) {
  return (
    <div
      data-slot="markdown-preview"
      className={cn("min-h-0 overflow-auto px-16 py-12 text-13 text-text", className)}
    >
      <ReactMarkdown skipHtml rehypePlugins={[rehypeSanitize]} components={COMPONENTS}>
        {markdown}
      </ReactMarkdown>
    </div>
  );
}
