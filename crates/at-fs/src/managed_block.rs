//! Format-preserving renderer for AgenTuna-owned instruction regions.
//!
//! The markers are only recognised as markers when they occupy a line of their own and are
//! not inside a Markdown fenced code block. That way a rule file may document the format
//! (`Use <!-- agentuna:begin --> to start`, or a fenced example) without the renderer
//! treating the example as the real block and rewriting the user's text around it.

use std::path::{Path, PathBuf};

/// Start marker for an AgenTuna-managed instruction block.
pub const MANAGED_BEGIN: &[u8] = b"<!-- agentuna:begin -->";
/// End marker for an AgenTuna-managed instruction block.
pub const MANAGED_END: &[u8] = b"<!-- agentuna:end -->";

/// Why rendering a managed block was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedBlockError {
    /// Existing unmarked file needs explicit adoption.
    #[error("adoption is required before modifying {path}")]
    AdoptRequired {
        /// Existing file.
        path: PathBuf,
    },
    /// Markers are invalid or unterminated.
    #[error("malformed managed block in {path} at line {line}")]
    MalformedManagedBlock {
        /// Invalid file.
        path: PathBuf,
        /// One-based start line.
        line: usize,
    },
}

/// Byte offsets of one managed block, as detected by [`managed_span`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSpan {
    /// Offset of the begin marker.
    pub begin: usize,
    /// Offset just after the begin marker; the body starts here (its first byte is the
    /// begin-marker line's ending).
    pub body_start: usize,
    /// Offset of the start of the end marker's line. Everything from here belongs to the
    /// suffix, including any indentation before the end marker.
    pub body_end: usize,
    /// Offset of the end marker.
    pub end: usize,
}

/// Finds the single managed block in `bytes`.
///
/// Recognised markers sit on a line of their own outside a Markdown fenced code block. A
/// marker inside a fence or inline code is user text: it is never reported here, so the
/// renderer and the rule editor never mistake a documented example for the real block.
///
/// # Errors
/// Returns [`ManagedBlockError::AdoptRequired`] when no active marker exists, and
/// [`ManagedBlockError::MalformedManagedBlock`] for a missing end marker, a stray marker, a
/// second begin marker or a second end marker.
pub fn managed_span(path: &Path, bytes: &[u8]) -> Result<ManagedSpan, ManagedBlockError> {
    let markers = active_markers(bytes);
    for (index, (position, is_begin)) in markers.iter().enumerate() {
        if index >= 2 || *is_begin != (index == 0) {
            return Err(ManagedBlockError::MalformedManagedBlock {
                path: path.to_path_buf(),
                line: line_of(bytes, *position),
            });
        }
    }
    let Some((begin, _)) = markers.first() else {
        return Err(ManagedBlockError::AdoptRequired {
            path: path.to_path_buf(),
        });
    };
    let Some((end, _)) = markers.get(1) else {
        return Err(ManagedBlockError::MalformedManagedBlock {
            path: path.to_path_buf(),
            line: line_of(bytes, *begin),
        });
    };
    Ok(ManagedSpan {
        begin: *begin,
        body_start: begin + MANAGED_BEGIN.len(),
        body_end: line_start(bytes, *end),
        end: *end,
    })
}

/// Replaces only the managed content. An absent file becomes a managed block.
///
/// Markers are detected by [`managed_span`]. Everything before and including the begin marker
/// and from the start of the end marker's line onward is copied byte for byte. The block body
/// uses the line ending of its own begin-marker line (LF when there is none), and `content` is
/// normalised to that ending, so a CRLF file stays CRLF.
///
/// # Errors
/// Returns [`ManagedBlockError::AdoptRequired`] for an existing unmarked file or
/// [`ManagedBlockError::MalformedManagedBlock`] for invalid marker placement.
pub fn render_managed_block(
    path: &Path,
    existing: Option<&[u8]>,
    content: &[u8],
) -> Result<Vec<u8>, ManagedBlockError> {
    let Some(existing) = existing else {
        let mut out =
            Vec::with_capacity(content.len() + MANAGED_BEGIN.len() + MANAGED_END.len() + 4);
        out.extend_from_slice(MANAGED_BEGIN);
        out.extend_from_slice(b"\n");
        out.extend_from_slice(content);
        if !content.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(MANAGED_END);
        out.push(b'\n');
        return Ok(out);
    };
    let span = managed_span(path, existing)?;
    let newline = line_ending_after(existing, span.body_start);
    let mut out = Vec::with_capacity(existing.len() + content.len());
    out.extend_from_slice(&existing[..span.body_start]);
    out.extend_from_slice(newline);
    out.extend_from_slice(&rewrite_newlines(content, newline));
    if !out.ends_with(newline) {
        out.extend_from_slice(newline);
    }
    out.extend_from_slice(&existing[span.body_end..]);
    Ok(out)
}

/// Marker occurrences that are real markers: alone on their line, outside a fenced block.
fn active_markers(bytes: &[u8]) -> Vec<(usize, bool)> {
    let mut active = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    for (start, end) in line_spans(bytes) {
        let content = &bytes[start..end];
        if let Some((character, length)) = fence {
            if closes_fence(content, character, length) {
                fence = None;
            }
            continue;
        }
        if let Some(opening) = opens_fence(content) {
            fence = Some(opening);
            continue;
        }
        let trimmed = trim_ascii_space(content);
        if trimmed == MANAGED_BEGIN {
            active.push((start + leading_space_len(content), true));
        } else if trimmed == MANAGED_END {
            active.push((start + leading_space_len(content), false));
        }
    }
    active
}

/// The content range of every line, excluding `\n` and a preceding `\r`.
fn line_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            let mut end = index;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            spans.push((start, end));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        spans.push((start, bytes.len()));
    }
    spans
}

/// Byte position where the line containing `position` starts.
fn line_start(bytes: &[u8], position: usize) -> usize {
    bytes[..position]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1)
}

/// One-based line number of `position`.
fn line_of(bytes: &[u8], position: usize) -> usize {
    1 + bytes[..position]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

/// The line ending used on the begin-marker line: `\r\n` only when that next newline is one.
fn line_ending_after(bytes: &[u8], position: usize) -> &'static [u8] {
    match bytes[position..].iter().position(|byte| *byte == b'\n') {
        Some(index) if index > 0 && bytes[position + index - 1] == b'\r' => b"\r\n",
        Some(_) => b"\n",
        None => b"\n",
    }
}

/// Rewrites every line ending in `content` to `newline`.
fn rewrite_newlines(content: &[u8], newline: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    let mut index = 0;
    while index < content.len() {
        match content[index] {
            b'\r' if content.get(index + 1) == Some(&b'\n') => {
                out.extend_from_slice(newline);
                index += 2;
            }
            b'\n' => {
                out.extend_from_slice(newline);
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

/// Length of the leading spaces and tabs in `line`.
fn leading_space_len(line: &[u8]) -> usize {
    line.iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

/// `line` without leading and trailing spaces and tabs.
fn trim_ascii_space(line: &[u8]) -> &[u8] {
    let leading = leading_space_len(line);
    let trailing = line[leading..]
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(0, |index| index + 1);
    &line[leading..leading + trailing]
}

/// The fence character and run length when `line` opens a fenced code block.
///
/// A fence is indented by at most three spaces (Markdown's rule); a tab or four spaces opens an
/// indented code block instead and is not tracked. A backtick fence's info string cannot
/// contain a backtick.
fn opens_fence(line: &[u8]) -> Option<(u8, usize)> {
    let indentation = line.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let rest = &line[indentation..];
    let character = *rest.first()?;
    if !matches!(character, b'`' | b'~') {
        return None;
    }
    let length = rest.iter().take_while(|byte| **byte == character).count();
    if length < 3 {
        return None;
    }
    let info = &rest[length..];
    if character == b'`' && info.contains(&b'`') {
        return None;
    }
    Some((character, length))
}

/// `true` when `line` closes a fence opened with `character` repeated `length` times.
fn closes_fence(line: &[u8], character: u8, length: usize) -> bool {
    let trimmed = line
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(&line[..0], |index| &line[..=index]);
    let indentation = trimmed.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return false;
    }
    let run = trimmed[indentation..]
        .iter()
        .take_while(|byte| **byte == character)
        .count();
    run >= length && trimmed[indentation + run..].is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(existing: &str, content: &str) -> String {
        String::from_utf8(
            render_managed_block(
                Path::new("AGENTS.md"),
                Some(existing.as_bytes()),
                content.as_bytes(),
            )
            .expect("render"),
        )
        .expect("utf8")
    }

    #[test]
    fn replaces_only_the_block_body() {
        let existing = "top\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\nbottom\n";
        assert_eq!(
            render(existing, "new"),
            "top\n<!-- agentuna:begin -->\nnew\n<!-- agentuna:end -->\nbottom\n"
        );
    }

    #[test]
    fn marker_examples_in_fences_and_inline_code_are_user_text() {
        let fenced = "```markdown\n<!-- agentuna:begin -->\n```\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\ntail\n";
        assert_eq!(
            render(fenced, "new"),
            "```markdown\n<!-- agentuna:begin -->\n```\n<!-- agentuna:begin -->\nnew\n<!-- agentuna:end -->\ntail\n"
        );
        let inline = "Use `<!-- agentuna:begin -->` to start.\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\ntail\n";
        assert_eq!(
            render(inline, "new"),
            "Use `<!-- agentuna:begin -->` to start.\n<!-- agentuna:begin -->\nnew\n<!-- agentuna:end -->\ntail\n"
        );
    }

    #[test]
    fn crlf_files_keep_crlf_inside_the_block() {
        let existing =
            "top\r\n<!-- agentuna:begin -->\r\nold\r\n<!-- agentuna:end -->\r\nbottom\r\n";
        assert_eq!(
            render(existing, "new\nline\n"),
            "top\r\n<!-- agentuna:begin -->\r\nnew\r\nline\r\n<!-- agentuna:end -->\r\nbottom\r\n"
        );
    }

    #[test]
    fn indented_end_marker_keeps_its_indentation() {
        let existing = "list\n  <!-- agentuna:begin -->\n  old\n  <!-- agentuna:end -->\n";
        assert_eq!(
            render(existing, "new"),
            "list\n  <!-- agentuna:begin -->\nnew\n  <!-- agentuna:end -->\n"
        );
    }

    #[test]
    fn user_text_without_markers_requires_adoption() {
        let error = render_managed_block(Path::new("AGENTS.md"), Some(b"# plain\n"), b"content")
            .expect_err("unmarked");
        assert!(matches!(error, ManagedBlockError::AdoptRequired { .. }));
    }

    #[test]
    fn malformed_marker_sets_are_reported_with_their_line() {
        let cases: &[(&str, usize)] = &[
            ("head\n<!-- agentuna:begin -->\nbody\n", 2),
            ("<!-- agentuna:end -->\n", 1),
            (
                "<!-- agentuna:begin -->\n<!-- agentuna:begin -->\n<!-- agentuna:end -->\n",
                2,
            ),
            (
                "<!-- agentuna:begin -->\n<!-- agentuna:end -->\n<!-- agentuna:end -->\n",
                3,
            ),
        ];
        for (existing, line) in cases {
            let error = render_managed_block(
                Path::new("AGENTS.md"),
                Some(existing.as_bytes()),
                b"content",
            )
            .expect_err("malformed");
            assert!(
                matches!(error, ManagedBlockError::MalformedManagedBlock { line: actual, .. } if actual == *line),
                "{existing:?} -> {error:?}"
            );
        }
    }
}
