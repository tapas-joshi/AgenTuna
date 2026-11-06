//! Group native copies and identify content conflicts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use at_model::{AgentId, ItemKind, Scope};
use similar::{ChangeTag, TextDiff};

use crate::scan::{ParseStatus, ScanItem, ScanReport};

/// A native copy participating in a conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictCopy {
    /// Agent that reads this copy.
    pub agent: AgentId,
    /// Native path.
    pub path: PathBuf,
    /// Native scope.
    pub scope: Scope,
    /// Hash as read by the scanner.
    pub raw_hash: String,
    /// Hash after item-kind normalization.
    pub normalized_hash: String,
}

/// Suggested action after comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProposedResolution {
    /// Keep the first copy; only formatting differs.
    KeepA,
    /// User must explicitly choose a substantive content conflict.
    NeedsChoice,
}

/// One group of conflicting copies for a normalized item name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// Item kind.
    pub kind: ItemKind,
    /// Normalized display name.
    pub name: String,
    /// Compared source copies in stable path order.
    pub copies: Vec<ConflictCopy>,
    /// Whether raw differences disappear after normalization.
    pub formatting_only: bool,
    /// Text diff. MCP values are not rendered to avoid disclosing credentials.
    pub unified_diff: String,
    /// Default action, if any.
    pub proposed_resolution: ProposedResolution,
}

/// All substantive conflicts and formatting-only groups discovered in one scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConflictSet {
    /// Groups containing two or more distinct copies.
    pub conflicts: Vec<Conflict>,
}

/// Compares native copies by kind and normalized name.
///
/// # Errors
/// Never fails: unreadable or malformed scan rows are ignored because they cannot be compared.
#[must_use]
pub fn detect_conflicts(scan: &ScanReport) -> ConflictSet {
    let mut groups: BTreeMap<(ItemKind, String), Vec<&ScanItem>> = BTreeMap::new();
    for item in &scan.items {
        if !matches!(item.parse_status, ParseStatus::Parsed) {
            continue;
        }
        groups
            .entry((item.kind, normalize(&item.name)))
            .or_default()
            .push(item);
    }
    let mut conflicts = Vec::new();
    for ((kind, name), mut items) in groups {
        items.sort_by(|left, right| left.path.cmp(&right.path));
        items.dedup_by(|left, right| left.path == right.path);
        if items.len() < 2 {
            continue;
        }
        let raw_hashes: BTreeSet<&str> = items
            .iter()
            .filter_map(|item| item.content_hash.as_deref())
            .collect();
        let normalized_hashes: BTreeSet<&str> = items
            .iter()
            .filter_map(|item| item.normalized_hash.as_deref())
            .collect();
        if raw_hashes.len() < 2 && normalized_hashes.len() < 2 {
            continue;
        }
        let formatting_only = normalized_hashes.len() == 1;
        let copies = items
            .iter()
            .filter_map(|item| {
                Some(ConflictCopy {
                    agent: item.agents.iter().next()?.clone(),
                    path: item.path.clone(),
                    scope: item.scope,
                    raw_hash: item.content_hash.clone()?,
                    normalized_hash: item.normalized_hash.clone()?,
                })
            })
            .collect::<Vec<_>>();
        if copies.len() < 2 {
            continue;
        }
        let mcp = kind == ItemKind::McpServer;
        let diff = if formatting_only {
            String::new()
        } else if mcp {
            format!(
                "--- {}\\n+++ {}\\n@@ {} (redacted) @@\\n server definition differs; values are intentionally redacted\\n",
                copies[0].path.display(),
                copies[1].path.display(),
                name
            )
        } else {
            text_diff(&copies[0], &copies[1])
        };
        conflicts.push(Conflict {
            kind,
            name,
            copies,
            formatting_only,
            unified_diff: diff,
            proposed_resolution: if formatting_only {
                ProposedResolution::KeepA
            } else {
                ProposedResolution::NeedsChoice
            },
        });
    }

    ConflictSet { conflicts }
}

/// Picks two non-colliding names for KeepBoth, or returns `None` if targets are exhausted.
#[must_use]
pub fn keep_both_names(
    name: &str,
    agent: &AgentId,
    existing: &BTreeSet<String>,
) -> Option<(String, String)> {
    let agent_name = format!(
        "{}-{}",
        name,
        agent.to_string().to_lowercase().replace('_', "-")
    );
    if !valid_name(&agent_name) || existing.contains(&agent_name) {
        return None;
    }
    for suffix in 2..=9999 {
        let second = format!("{name}-{suffix}");
        if valid_name(&second) && second != agent_name && !existing.contains(&second) {
            return Some((agent_name, second));
        }
    }
    None
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace('_', "-")
}

fn text_diff(a: &ConflictCopy, b: &ConflictCopy) -> String {
    let a_text = std::fs::read_to_string(&a.path).unwrap_or_default();
    let b_text = std::fs::read_to_string(&b.path).unwrap_or_default();
    let diff = TextDiff::from_lines(&a_text, &b_text);
    let mut output = format!("--- {}\n+++ {}\n", a.path.display(), b.path.display());
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => output.push('-'),
            ChangeTag::Insert => output.push('+'),
            ChangeTag::Equal => output.push(' '),
        }
        output.push_str(change.value());
        if !change.value().ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Ownership, ScanItem};

    fn copy(path: &str, raw: &str, normalized: &str, agent: AgentId) -> ScanItem {
        ScanItem {
            kind: ItemKind::Rule,
            name: "rules".to_owned(),
            agents: BTreeSet::from([agent]),
            path: PathBuf::from(path),
            scope: Scope::Project,
            content_hash: Some(raw.to_owned()),
            normalized_hash: Some(normalized.to_owned()),
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
        }
    }

    #[test]
    fn equal_normalized_copies_are_formatting_only() {
        let report = ScanReport {
            items: vec![
                copy("a.md", "raw-a", "same", AgentId::ClaudeCode),
                copy("b.md", "raw-b", "same", AgentId::Codex),
            ],
            partial: false,
            skipped_roots: Vec::new(),
        };
        let set = detect_conflicts(&report);
        assert_eq!(set.conflicts.len(), 1);
        assert!(set.conflicts[0].formatting_only);
        assert_eq!(
            set.conflicts[0].proposed_resolution,
            ProposedResolution::KeepA
        );
    }

    #[test]
    fn identical_copies_collapse_without_a_conflict() {
        let report = ScanReport {
            items: vec![
                copy("a.md", "same", "same", AgentId::ClaudeCode),
                copy("b.md", "same", "same", AgentId::Codex),
            ],
            partial: false,
            skipped_roots: Vec::new(),
        };
        assert!(detect_conflicts(&report).conflicts.is_empty());
    }
    #[test]
    fn keep_both_names_avoids_store_and_agent_collisions_deterministically() {
        let agent_suffix = format!(
            "rule-{}",
            AgentId::ClaudeCode
                .to_string()
                .to_lowercase()
                .replace('_', "-")
        );
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from(["rule-2".to_owned()])
            ),
            Some((agent_suffix.clone(), "rule-3".to_owned()))
        );
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from([agent_suffix.clone()])
            ),
            None
        );
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from([agent_suffix, "rule-2".to_owned()])
            ),
            None
        );
    }
}
