//! Group native copies and identify content conflicts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use at_model::{AgentId, ItemKind, Scope};
use similar::{ChangeTag, TextDiff};

use crate::scan::{Ownership, ParseStatus, ScanItem, ScanReport, StoreItem};

/// Where a compared copy lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CopyOrigin {
    /// A copy discovered in an agent's native folder.
    Native,
    /// The canonical copy in the AgenTuna store.
    Store,
}

/// One compared copy of an item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConflictCopy {
    /// Agent that reads this copy; `None` for the canonical store copy.
    pub agent: Option<AgentId>,
    /// Whether this copy lives in an agent folder or the canonical store.
    pub origin: CopyOrigin,
    /// Path of this copy: the native entry or the store item.
    pub path: PathBuf,
    /// Scope of this copy.
    pub scope: Scope,
    /// Hash as read by the scanner.
    pub raw_hash: String,
    /// Hash after item-kind normalization.
    pub normalized_hash: String,
}

/// Suggested action after comparison.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ProposedResolution {
    /// Keep the first copy; only formatting differs.
    KeepA,
    /// User must explicitly choose a substantive content conflict.
    NeedsChoice,
}

/// One group of conflicting copies for a normalized item name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    /// Item kind.
    pub kind: ItemKind,
    /// Normalized display name.
    pub name: String,
    /// Compared copies. Native copies are ordered by path; a canonical store copy, when the
    /// conflict is native-against-store, is always the last copy.
    pub copies: Vec<ConflictCopy>,
    /// Whether raw differences disappear after normalization.
    pub formatting_only: bool,
    /// Text diff. MCP values are not rendered to avoid disclosing credentials.
    pub unified_diff: String,
    /// Default action, if any.
    pub proposed_resolution: ProposedResolution,
}

/// Copies that share one raw and normalized hash, so they need no review.
///
/// Two agents holding byte-identical copies of one item produce one equal group and no
/// [`Conflict`] row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EqualGroup {
    /// Item kind.
    pub kind: ItemKind,
    /// Normalized display name.
    pub name: String,
    /// Identical copies in stable path order.
    pub copies: Vec<ConflictCopy>,
}

/// A native copy whose normalized content differs from the canonical store item appears in
/// [`ConflictSet::conflicts`] as a two-copy group: copy A is the native copy and copy B is the
/// canonical store copy (its [`ConflictCopy::origin`] is [`CopyOrigin::Store`]). Keeping A
/// stores the native content under the same name; keeping B leaves the store item untouched and
/// migrates nothing.
///
/// All substantive conflicts and formatting-only groups discovered in one scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConflictSet {
    /// Groups that differ after normalization and need a decision.
    pub conflicts: Vec<Conflict>,
    /// Groups whose copies are byte-identical, so no review row is produced.
    pub equal_groups: Vec<EqualGroup>,
}

/// Compares native copies with each other by kind and normalized name.
///
/// Never fails: unreadable or malformed scan rows are ignored because they cannot be compared.
#[must_use]
pub fn detect_conflicts(scan: &ScanReport) -> ConflictSet {
    detect(scan, None)
}

/// Compares native copies with each other and with the canonical store inventory.
///
/// One native copy that differs from the matching store item becomes a two-copy conflict whose
/// copy B is the store item, so a migration cannot silently overwrite reviewed store content.
/// Copies that share one raw and normalized hash collapse into a single [`EqualGroup`] and never
/// produce a conflict row.
#[must_use]
pub fn detect_conflicts_with_store(scan: &ScanReport, store_items: &[StoreItem]) -> ConflictSet {
    detect(scan, Some(store_items))
}

fn detect(scan: &ScanReport, store_items: Option<&[StoreItem]>) -> ConflictSet {
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
    let mut set = ConflictSet::default();
    let mut native_conflicts: BTreeSet<(ItemKind, String)> = BTreeSet::new();
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
            let copies = copies_of(&items);
            if copies.len() >= 2 {
                set.equal_groups.push(EqualGroup { kind, name, copies });
            }
            continue;
        }
        let formatting_only = normalized_hashes.len() == 1;
        let copies = copies_of(&items);
        if copies.len() < 2 {
            continue;
        }
        let unified_diff = if formatting_only {
            String::new()
        } else {
            conflict_diff(kind, &name, &copies)
        };
        native_conflicts.insert((kind, name.clone()));
        set.conflicts.push(Conflict {
            kind,
            name,
            copies,
            formatting_only,
            unified_diff,
            proposed_resolution: if formatting_only {
                ProposedResolution::KeepA
            } else {
                ProposedResolution::NeedsChoice
            },
        });
    }
    if let Some(store_items) = store_items {
        for item in &scan.items {
            if !matches!(item.parse_status, ParseStatus::Parsed)
                || item.ownership != Ownership::Conflict
            {
                continue;
            }
            let name = normalize(&item.name);
            if native_conflicts.contains(&(item.kind, name.clone())) {
                continue;
            }
            let Some(store) = store_items
                .iter()
                .find(|entry| entry.kind == item.kind && normalize(&entry.name) == name)
            else {
                continue;
            };
            let Some(native) = copy_of(item) else {
                continue;
            };
            let store_copy = ConflictCopy {
                agent: None,
                origin: CopyOrigin::Store,
                path: store.path.clone(),
                scope: store.scope,
                raw_hash: store.content_hash.clone(),
                normalized_hash: store.content_hash.clone(),
            };
            let copies = vec![native, store_copy];
            set.conflicts.push(Conflict {
                kind: item.kind,
                name: name.clone(),
                unified_diff: conflict_diff(item.kind, &name, &copies),
                copies,
                formatting_only: false,
                proposed_resolution: ProposedResolution::NeedsChoice,
            });
        }
    }
    set
}

/// Names already taken in the store and in every enabled agent folder for `kind`.
///
/// Agent folders are represented by the scan itself: each parsed row is one name an agent
/// already resolves, and the store inventory contributes the canonical names. Keep-both naming
/// runs against this set so a generated name cannot collide in either place.
#[must_use]
pub fn occupied_names(
    scan: &ScanReport,
    store_items: &[StoreItem],
    kind: ItemKind,
) -> BTreeSet<String> {
    scan.items
        .iter()
        .filter(|item| item.kind == kind && !item.name.trim().is_empty())
        .map(|item| item.name.clone())
        .chain(
            store_items
                .iter()
                .filter(|entry| entry.kind == kind)
                .map(|entry| entry.name.clone()),
        )
        .collect()
}

/// One copy per comparable scan row.
fn copies_of(items: &[&ScanItem]) -> Vec<ConflictCopy> {
    items.iter().filter_map(|item| copy_of(item)).collect()
}

/// The reviewed copy of a scan row, or `None` when the scanner recorded no digests.
fn copy_of(item: &ScanItem) -> Option<ConflictCopy> {
    Some(ConflictCopy {
        agent: item.agents.iter().next().cloned(),
        origin: CopyOrigin::Native,
        path: item.path.clone(),
        scope: item.scope,
        raw_hash: item.content_hash.clone()?,
        normalized_hash: item.normalized_hash.clone()?,
    })
}

/// A diff that stays useful without disclosing credentials.
///
/// MCP entries contain environment values, so only the shape of the change is rendered; skill
/// trees are directories, so the differing file names are listed instead of file contents.
fn conflict_diff(kind: ItemKind, name: &str, copies: &[ConflictCopy]) -> String {
    let first = &copies[0];
    let second = &copies[1];
    match kind {
        ItemKind::McpServer => format!(
            "--- {}\n+++ {}\n@@ {} (redacted) @@\n server definition differs; values are intentionally redacted\n",
            first.path.display(),
            second.path.display(),
            name
        ),
        ItemKind::Skill => {
            let mut output = format!(
                "--- {}\n+++ {}\n skill tree content differs\n",
                first.path.display(),
                second.path.display()
            );
            for relative in differing_tree_entries(&first.path, &second.path) {
                output.push(' ');
                output.push_str(&relative);
                output.push('\n');
            }
            output
        }
        _ => text_diff(&first.path, &second.path),
    }
}

/// Files that differ between two skill trees, relative to the tree root and sorted.
///
/// Unreadable trees yield no names: the conflict still needs a decision, and the diff body then
/// states only that the trees differ.
fn differing_tree_entries(first: &std::path::Path, second: &std::path::Path) -> Vec<String> {
    let (Ok(first_files), Ok(second_files)) = (tree_files(first), tree_files(second)) else {
        return Vec::new();
    };
    let mut differing = BTreeSet::new();
    for (relative, hash) in &first_files {
        if second_files.get(relative) != Some(hash) {
            differing.insert(relative.clone());
        }
    }
    for relative in second_files.keys() {
        if !first_files.contains_key(relative) {
            differing.insert(relative.clone());
        }
    }
    differing.into_iter().collect()
}

fn tree_files(root: &std::path::Path) -> std::io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative, at_fs::sha256_hex(&std::fs::read(&path)?));
            }
        }
    }
    Ok(files)
}

/// Picks two non-colliding names for KeepBoth, or returns `None` if targets are exhausted.
///
/// The first name is `<name>-<agent>`; the second is `<name>-2`. Either gains a numeric suffix
/// when it already exists, so a store collision, an agent collision or both still resolve:
/// with `rule-2` taken the second name becomes `rule-3`, and with `rule-claude-code` taken the
/// first becomes `rule-claude-code-2`.
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
    let first = unique_name(&agent_name, existing)?;
    let mut taken = existing.clone();
    taken.insert(first.clone());
    let second = unique_name_with_start(name, 2, &taken)?;
    Some((first, second))
}

/// Returns `base` when free, otherwise the first free `<base>-<n>` (`n` from 2).
fn unique_name(base: &str, existing: &BTreeSet<String>) -> Option<String> {
    if valid_name(base) && !existing.contains(base) {
        return Some(base.to_owned());
    }
    unique_name_with_start(base, 2, existing)
}

/// Returns the first free `<base>-<n>` with `n` starting at `start`.
fn unique_name_with_start(base: &str, start: usize, existing: &BTreeSet<String>) -> Option<String> {
    for suffix in start..=9999 {
        let candidate = format!("{base}-{suffix}");
        if valid_name(&candidate) && !existing.contains(&candidate) {
            return Some(candidate);
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

fn text_diff(a: &std::path::Path, b: &std::path::Path) -> String {
    let a_text = std::fs::read_to_string(a).unwrap_or_default();
    let b_text = std::fs::read_to_string(b).unwrap_or_default();
    let diff = TextDiff::from_lines(&a_text, &b_text);
    let mut output = format!("--- {}\n+++ {}\n", a.display(), b.display());
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
    use crate::scan::DiscoveryState;
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
            mcp_summary: None,
            parse_status: ParseStatus::Parsed,
            store_link: false,
            ownership: Ownership::NotInAgenTuna,
            discovery: DiscoveryState::Valid,
            link_target: None,
            resolved: None,
        }
    }

    #[test]
    fn equal_normalized_copies_are_formatting_only() {
        let report = ScanReport {
            roots: Vec::new(),
            items: vec![
                copy("a.md", "raw-a", "same", AgentId::ClaudeCode),
                copy("b.md", "raw-b", "same", AgentId::Codex),
            ],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
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
    fn identical_copies_collapse_into_one_equal_group() {
        let report = ScanReport {
            roots: Vec::new(),
            items: vec![
                copy("a.md", "same", "same", AgentId::ClaudeCode),
                copy("b.md", "same", "same", AgentId::Codex),
            ],
            partial: false,
            skipped_roots: Vec::new(),
            warnings: Vec::new(),
        };
        let set = detect_conflicts(&report);
        assert!(set.conflicts.is_empty(), "identical copies need no review");
        assert_eq!(set.equal_groups.len(), 1, "one group for one shared hash");
        assert_eq!(set.equal_groups[0].copies.len(), 2);
        assert_eq!(set.equal_groups[0].name, "rules");
    }
    #[test]
    fn keep_both_names_resolves_store_agent_and_combined_collisions() {
        let agent_suffix = format!(
            "rule-{}",
            AgentId::ClaudeCode
                .to_string()
                .to_lowercase()
                .replace('_', "-")
        );
        assert_eq!(
            keep_both_names("rule", &AgentId::ClaudeCode, &BTreeSet::new()),
            Some((agent_suffix.clone(), "rule-2".to_owned()))
        );
        // Store collision: `rule-2` is taken, so the second name moves to `rule-3`.
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from(["rule-2".to_owned()])
            ),
            Some((agent_suffix.clone(), "rule-3".to_owned()))
        );
        // Agent collision: the agent-suffixed name is taken, so it gains its own suffix.
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from([agent_suffix.clone()])
            ),
            Some((format!("{agent_suffix}-2"), "rule-2".to_owned()))
        );
        // Both: the agent name gains `-2` and the numeric name moves to `rule-3`.
        assert_eq!(
            keep_both_names(
                "rule",
                &AgentId::ClaudeCode,
                &BTreeSet::from([agent_suffix.clone(), "rule-2".to_owned()])
            ),
            Some((format!("{agent_suffix}-2"), "rule-3".to_owned()))
        );
    }
}
