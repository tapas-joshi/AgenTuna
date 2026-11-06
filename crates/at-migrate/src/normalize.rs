//! Content normalization shared by native scanning and conflict classification.
//!
//! Two copies of one native item are a conflict only when their *normalized* content
//! differs. Normalization is deliberately conservative and kind-specific: line endings and
//! trailing whitespace in rules, CRLF and frontmatter field order in `SKILL.md`, and JSON key
//! order in MCP entries are formatting; every other byte is content. The scanner hashes with
//! these functions and [`crate::conflicts`] compares the results, so one implementation decides
//! what "the same item" means.

use std::path::Path;

use at_store::manifest::{ContentHash, hash_mcp_entry, hash_rule_bytes, hash_skill_folder};
use serde_json::Value;

/// Normalized digest of a rule file.
///
/// `\r\n` becomes `\n`, trailing spaces and tabs are removed from every line, and trailing
/// newlines at the end of the file are dropped. A lone `\r` is content.
#[must_use]
pub fn rule_bytes(bytes: &[u8]) -> ContentHash {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let normalized = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    hash_rule_bytes(normalized.trim_end().as_bytes())
}

/// Normalized digest of a skill folder: `SKILL.md` is normalized, other files stay byte-exact.
///
/// # Errors
/// [`at_store::manifest::HashError`] for a missing, unreadable, oversized or symlinked tree.
pub fn skill_tree(root: &Path) -> Result<ContentHash, at_store::manifest::HashError> {
    hash_skill_folder(root)
}

/// Normalized digest of one MCP server definition: canonical JSON with sorted keys.
///
/// `${VAR}` references stay literal, so two references to the same variable are equal while a
/// different variable name is content.
///
/// # Errors
/// [`at_store::manifest::CanonicalJsonError`] when a string cannot be rendered.
pub fn mcp_entry(value: &Value) -> Result<ContentHash, at_store::manifest::CanonicalJsonError> {
    hash_mcp_entry(value)
}

#[cfg(test)]
mod tests {
    use super::rule_bytes;

    #[test]
    fn rule_normalization_ignores_line_endings_and_trailing_spaces() {
        assert_eq!(
            rule_bytes(b"# Rule  \r\ncontent\t\r\n\r\n"),
            rule_bytes(b"# Rule\ncontent\n")
        );
        assert_ne!(
            rule_bytes(b"# Rule\ncontent\n"),
            rule_bytes(b"# Rule\nother\n")
        );
        assert_ne!(rule_bytes(b"a\rb\n"), rule_bytes(b"a\nb\n"));
    }
}
