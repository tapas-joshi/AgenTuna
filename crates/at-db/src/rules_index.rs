//! Discovered rule-file projection over the canonical item and version tables.

use sha2::{Digest, Sha256};
use std::path::Path;

use at_model::{ItemId, Scope};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    DbError,
    versions::{NewVersion, record_skill_version},
};

/// A scanned rule file supplied by the filesystem scanner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannedRule {
    /// Scope where the file was discovered.
    pub scope: Scope,
    /// Project identity for Project and Local rules; otherwise `None`.
    pub project_id: Option<String>,
    /// Adapter identity that reads the file.
    pub agent: String,
    /// On-disk path.
    pub path: String,
    /// Optional resolved symlink target.
    pub link_target: Option<String>,
    /// Exact bytes.
    pub bytes: Vec<u8>,
    /// Unix modification time in milliseconds.
    pub mtime: Option<i64>,
    /// Whether AgenTuna owns the managed block.
    pub managed: bool,
}

/// Rule item projection returned for a selected scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleIndexRow {
    /// Stable item identifier.
    pub item_id: ItemId,
    /// Scope of the rule.
    pub scope: Scope,
    /// Adapter identity.
    pub agent: String,
    /// File path.
    pub path: String,
    /// Whether the rule is AgenTuna-managed.
    pub managed: bool,
    /// Resolved symlink target.
    pub link_target: Option<String>,
    /// SHA-256 of the current bytes.
    pub sha256: String,
    /// Current state (`present` or `missing`).
    pub state: String,
}

/// Inserts or refreshes one rule projection and its version observation.
///
/// # Errors
/// Returns [`DbError`] for SQL failures or invalid stored identifiers.
pub fn upsert_on_scan(connection: &Connection, rule: &ScannedRule) -> Result<ItemId, DbError> {
    let mut hash = String::with_capacity(64);
    for byte in Sha256::digest(&rule.bytes) {
        use std::fmt::Write as _;
        let _ = write!(hash, "{byte:02x}");
    }
    let name = Path::new(&rule.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rule");
    let id: Option<String> = connection.query_row(
        "SELECT id FROM items WHERE kind='rule' AND scope=?1 AND project_id IS ?2 \
         AND (source_path=?3 OR source_path=?4 OR link_target=?3 OR link_target=?4) ORDER BY id LIMIT 1",
        params![rule.scope.wire_name(), rule.project_id, rule.path, rule.link_target], |row| row.get(0),
    ).optional()?;
    let proposed_id = match id {
        Some(id) => id.parse::<ItemId>()?,
        None => ItemId::new(),
    };
    let tags = serde_json::json!({"agent": rule.agent}).to_string();
    let stored_id: String = connection.query_row(
        "INSERT INTO items (id,kind,scope,project_id,slug,name,source_path,link_target,content_hash,state,managed,tags_json,last_scanned_at) \
         VALUES (?1,'rule',?2,?3,?5,?4,?5,?6,?7,'present',?8,?9,strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
         ON CONFLICT DO UPDATE SET link_target=COALESCE(excluded.link_target,items.link_target),content_hash=excluded.content_hash,state='present',managed=excluded.managed,tags_json=excluded.tags_json,last_scanned_at=excluded.last_scanned_at,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') RETURNING id",
        params![proposed_id.to_string(), rule.scope.wire_name(), rule.project_id, name, rule.path, rule.link_target, hash, rule.managed, tags],
        |row| row.get(0),
    )?;
    let item_id = stored_id.parse::<ItemId>()?;
    let size = i64::try_from(rule.bytes.len()).map_err(|_| DbError::InvalidValue {
        field: "rule.bytes",
        reason: "size exceeds i64".to_owned(),
    })?;
    let path = rule.path.as_str();
    let mut version = NewVersion::new(&item_id, rule.scope, &hash).with_path(path);
    version.size = Some(size);
    version.mtime = rule.mtime;
    record_skill_version(connection, &version)?;
    connection.execute(
        "UPDATE item_versions SET last_seen_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
         WHERE item_id=?1 AND content_hash=?2",
        params![item_id.to_string(), hash],
    )?;
    Ok(item_id)
}

/// Returns rule files ordered by adapter then filename and path.
///
/// # Errors
/// Returns [`DbError`] for SQL failures or invalid stored identifiers/scopes.
pub fn rules_for_scope(
    connection: &Connection,
    scope: Scope,
    project_id: Option<&str>,
) -> Result<Vec<RuleIndexRow>, DbError> {
    let mut statement = connection.prepare(
        "SELECT i.id,i.scope,i.source_path,i.managed,i.link_target,i.content_hash,COALESCE(i.state,'missing'), \
         COALESCE(json_extract(i.tags_json,'$.agent'),'') \
         FROM items i WHERE i.kind='rule' AND i.scope=?1 AND i.project_id IS ?2 ORDER BY 8,lower(i.name),i.source_path",
    )?;
    let rows = statement.query_map(params![scope.wire_name(), project_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    rows.map(|row| {
        let (id, scope_text, path, managed, link_target, sha256, state, agent) = row?;
        Ok(RuleIndexRow {
            item_id: id.parse()?,
            scope: scope_text.parse()?,
            agent,
            path: path.unwrap_or_default(),
            managed,
            link_target,
            sha256: sha256.unwrap_or_default(),
            state,
        })
    })
    .collect()
}

/// Marks a rule path missing while preserving its item and version history.
///
/// # Errors
/// Returns [`DbError`] on SQL failure.
pub fn mark_missing(connection: &Connection, path: &str) -> Result<usize, DbError> {
    Ok(connection.execute("UPDATE items SET state='missing',updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE kind='rule' AND source_path=?1", [path])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn scan_upserts_stable_item_and_version_and_marks_missing() {
        let database = Database::open_in_memory().expect("db");
        let rule = ScannedRule {
            scope: Scope::Project,
            project_id: Some("project-1".into()),
            agent: "codex".into(),
            path: "/repo/AGENTS.md".into(),
            link_target: None,
            bytes: b"rules".to_vec(),
            mtime: Some(12),
            managed: true,
        };
        let first = upsert_on_scan(database.connection(), &rule).expect("first scan");
        let second = upsert_on_scan(database.connection(), &rule).expect("second scan");
        assert_eq!(first, second);
        let counts: (i64, i64) = database.connection().query_row(
            "SELECT (SELECT count(*) FROM items WHERE kind='rule'), (SELECT count(*) FROM item_versions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).expect("counts");
        assert_eq!(counts, (1, 1));
        let rows = rules_for_scope(database.connection(), Scope::Project, Some("project-1"))
            .expect("rules");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent, "codex");
        assert!(rows[0].managed);
        assert_eq!(
            mark_missing(database.connection(), "/repo/AGENTS.md").expect("missing"),
            1
        );
        assert_eq!(
            rules_for_scope(database.connection(), Scope::Project, Some("project-1"))
                .expect("rules")[0]
                .state,
            "missing"
        );
    }

    fn rule(path: &str, agent: &str, managed: bool) -> ScannedRule {
        ScannedRule {
            scope: Scope::Project,
            project_id: Some("project-1".into()),
            agent: agent.into(),
            path: path.into(),
            link_target: None,
            bytes: path.as_bytes().to_vec(),
            mtime: Some(7),
            managed,
        }
    }

    #[test]
    fn three_rule_files_insert_three_items_and_a_rescan_keeps_ids_and_updates_last_seen_only() {
        let database = Database::open_in_memory().expect("db");
        let connection = database.connection();
        let inputs = [
            rule("/repo/AGENTS.md", "codex", true),
            rule("/repo/CLAUDE.md", "claude_code", false),
            rule("/repo/.cursor/rules/style.mdc", "cursor", false),
        ];
        let mut first_ids = Vec::new();
        for input in &inputs {
            first_ids.push(upsert_on_scan(connection, input).expect("first scan"));
        }
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM items WHERE kind='rule'), \
                        (SELECT count(*) FROM item_versions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("counts");
        assert_eq!(counts, (3, 3), "one item and one version per rule file");

        connection
            .execute(
                "UPDATE item_versions SET last_seen_at='2000-01-01T00:00:00.000Z'",
                [],
            )
            .expect("backdate last_seen_at");
        let mut second_ids = Vec::new();
        for input in &inputs {
            second_ids.push(upsert_on_scan(connection, input).expect("rescan"));
        }
        assert_eq!(first_ids, second_ids, "rescan keeps every item id");
        let counts: (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM items WHERE kind='rule'), \
                        (SELECT count(*) FROM item_versions), \
                        (SELECT count(*) FROM item_versions \
                          WHERE last_seen_at='2000-01-01T00:00:00.000Z')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("counts after rescan");
        assert_eq!(
            counts,
            (3, 3, 0),
            "unchanged content adds no version and refreshes last_seen only"
        );
        let rows = rules_for_scope(connection, Scope::Project, Some("project-1"))
            .expect("rules for scope");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter()
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            ["claude_code", "codex", "cursor"],
            "rows are ordered by agent"
        );
        assert!(rows.iter().any(|row| row.managed));
    }

    #[test]
    fn a_symlink_and_its_target_resolve_to_one_row_with_link_target() {
        let database = Database::open_in_memory().expect("db");
        let connection = database.connection();
        let target = rule("/repo/AGENTS.md", "codex", false);
        let target_id = upsert_on_scan(connection, &target).expect("target scan");
        let mut link = rule("/repo/links/AGENTS.md", "codex", false);
        link.link_target = Some("/repo/AGENTS.md".into());
        let link_id = upsert_on_scan(connection, &link).expect("link scan");
        assert_eq!(target_id, link_id, "the link reuses the target's item id");
        let count: i64 = connection
            .query_row("SELECT count(*) FROM items WHERE kind='rule'", [], |row| {
                row.get(0)
            })
            .expect("item count");
        assert_eq!(count, 1, "the same real path is one row");
        let rows = rules_for_scope(connection, Scope::Project, Some("project-1"))
            .expect("rules for scope");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].link_target.as_deref(), Some("/repo/AGENTS.md"));
    }

    #[test]
    fn concurrent_upserts_of_the_same_natural_key_do_not_duplicate_rows() {
        let sandbox = tempfile::TempDir::new().expect("sandbox");
        let path = sandbox.path().join("history.sqlite");
        drop(Database::open_file(&path).expect("initialize schema"));
        let input = rule("/repo/AGENTS.md", "codex", true);
        let barrier = std::sync::Barrier::new(2);
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                let database = Database::open_file(&path).expect("watcher connection");
                barrier.wait();
                upsert_on_scan(database.connection(), &input).expect("watcher upsert")
            });
            let right = scope.spawn(|| {
                let database = Database::open_file(&path).expect("manual connection");
                barrier.wait();
                upsert_on_scan(database.connection(), &input).expect("manual upsert")
            });
            (left.join().expect("watcher"), right.join().expect("manual"))
        });
        assert_eq!(left, right, "both writers report the same item");
        let database = Database::open_file(&path).expect("verify connection");
        let counts: (i64, i64) = database
            .connection()
            .query_row(
                "SELECT (SELECT count(*) FROM items WHERE kind='rule'), \
                        (SELECT count(*) FROM item_versions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("counts");
        assert_eq!(counts, (1, 1), "one row despite two concurrent writers");
    }
}
