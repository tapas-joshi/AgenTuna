//! Unified diff rendering for write previews.

use similar::TextDiff;

/// Context lines around each change.
pub const CONTEXT_LINES: usize = 3;

/// Renders a unified diff of `old` (None = file absent) to `new` with 3 lines of context.
///
/// Non-UTF-8 content is not diffed line by line; a one-line note is returned instead.
/// The output contains file content, so callers must redact secrets before logging it.
#[must_use]
pub fn unified(display_path: &str, old: Option<&[u8]>, new: &[u8]) -> String {
    let old_text = match old {
        None => Some(""),
        Some(bytes) => std::str::from_utf8(bytes).ok(),
    };
    let (Some(old_text), Ok(new_text)) = (old_text, std::str::from_utf8(new)) else {
        return format!("Binary files {display_path} differ\n");
    };
    let old_header = if old.is_none() {
        "/dev/null".to_owned()
    } else {
        format!("a/{display_path}")
    };
    TextDiff::from_lines(old_text, new_text)
        .unified_diff()
        .context_radius(CONTEXT_LINES)
        .header(&old_header, &format!("b/{display_path}"))
        .to_string()
}
