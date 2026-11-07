//! TC-034: a reviewed dry run names exactly the files the same plan then writes.
//!
//! The test builds one mixed fixture (skill + MCP server + rule), projects it with
//! [`dry_run`], applies the identical plan, and compares the dry-run row set with the
//! `operation_files` rows the apply recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use at_db::Database;
use at_fs::ApplyRoots;
use at_migrate::apply::{ApplyPlanContext, apply_plan};
use at_migrate::conflicts::detect_conflicts;
use at_migrate::plan::{
    BuildPlanRequest, DryRunRow, MigrationAction, MigrationPlan, MigrationStrategy, build_plan,
    dry_run,
};
use at_migrate::scan::{Ownership, ParseStatus, ScanItem, ScanReport};
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: PathBuf,
    backups: PathBuf,
    items: Vec<ScanItem>,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().expect("temporary fixture");
    let root = fs::canonicalize(temp.path()).expect("canonical fixture root");
    let store = root.join("store");
    at_store::layout::init_store(Scope::User, &store).expect("initialised store");
    let backups = root.join("backups");
    fs::create_dir_all(&backups).expect("backup root");

    let skill_root = root.join("native/skills/sample-skill");
    fs::create_dir_all(&skill_root).expect("skill tree");
    fs::write(skill_root.join("SKILL.md"), b"# Sample\nbody\n").expect("skill document");
    let skill = ScanItem {
        kind: ItemKind::Skill,
        name: "sample-skill".to_owned(),
        agents: BTreeSet::from([AgentId::ClaudeCode]),
        path: skill_root,
        scope: Scope::User,
        content_hash: Some(at_fs::sha256_hex(b"# Sample\nbody\n")),
        normalized_hash: Some(
            at_store::manifest::hash_skill_folder(&root.join("native/skills/sample-skill"))
                .expect("skill hash")
                .to_string(),
        ),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };

    let mcp_path = root.join("native/mcp.json");
    let mcp_bytes = br#"{"mcpServers":{"example":{"command":"node","args":["server.js"]}}}"#;
    fs::write(&mcp_path, mcp_bytes).expect("mcp config");
    let mcp = ScanItem {
        kind: ItemKind::McpServer,
        name: "example".to_owned(),
        agents: BTreeSet::from([AgentId::ClaudeCode]),
        path: mcp_path,
        scope: Scope::User,
        content_hash: Some(at_fs::sha256_hex(mcp_bytes)),
        normalized_hash: Some(at_fs::sha256_hex(mcp_bytes)),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };

    let rule_path = root.join("native/team-rules.md");
    let rule_bytes = b"# Team rules\nbe kind\n";
    fs::write(&rule_path, rule_bytes).expect("rule file");
    let rule = ScanItem {
        kind: ItemKind::Rule,
        name: "team-rules".to_owned(),
        agents: BTreeSet::from([AgentId::ClaudeCode]),
        path: rule_path,
        scope: Scope::User,
        content_hash: Some(at_fs::sha256_hex(rule_bytes)),
        normalized_hash: Some(at_fs::sha256_hex(rule_bytes)),
        parse_status: ParseStatus::Parsed,
        store_link: false,
        ownership: Ownership::NotInAgenTuna,
    };

    Fixture {
        _temp: temp,
        root,
        store,
        backups,
        items: vec![skill, mcp, rule],
    }
}

fn plan(fixture: &Fixture, strategy: MigrationStrategy) -> MigrationPlan {
    let scan = ScanReport {
        items: fixture.items.clone(),
        partial: false,
        skipped_roots: Vec::new(),
        warnings: Vec::new(),
    };
    let conflicts = detect_conflicts(&scan);
    let selected = fixture
        .items
        .iter()
        .map(|item| item.path.clone())
        .collect::<BTreeSet<_>>();
    build_plan(BuildPlanRequest {
        scan: &scan,
        conflicts: &conflicts,
        resolutions: &BTreeMap::new(),
        selected_paths: &selected,
        store_root: fixture.store.clone(),
        copy_agents: &BTreeSet::new(),
        strategy,
        scope: Scope::User,
        backup_root: fixture.backups.clone(),
    })
    .expect("plan")
}

/// What a dry-run row set says will be written.
struct Planned {
    /// Exact file paths the plan names.
    files: BTreeSet<PathBuf>,
    /// Directories whose contents the plan creates (`CreateStoreItem`).
    trees: BTreeSet<PathBuf>,
}

/// Splits a dry-run row set into exact file destinations and created trees.
fn planned(rows: &[DryRunRow]) -> Planned {
    let mut files = BTreeSet::new();
    let mut trees = BTreeSet::new();
    for row in rows {
        match row.action {
            MigrationAction::CreateStoreItem => {
                if let Some(destination) = row.destination.as_ref() {
                    trees.insert(destination.clone());
                }
            }
            MigrationAction::UpdateManifest
            | MigrationAction::WriteLink
            | MigrationAction::WriteCopy => {
                if let Some(destination) = row.destination.as_ref() {
                    files.insert(destination.clone());
                }
            }
            // MCP and rule rendering writes the canonical store file *and* patches the native
            // source it was read from.
            MigrationAction::RenderMcp | MigrationAction::RenderRule => {
                if let Some(destination) = row.destination.as_ref() {
                    files.insert(destination.clone());
                }
                if let Some(source) = row.source.as_ref() {
                    files.insert(source.clone());
                }
            }
            // Backups and watched originals are metadata-only rows.
            _ => {}
        }
    }
    Planned { files, trees }
}

impl Planned {
    /// Whether `path` is covered by this plan, either exactly or inside a created tree.
    fn covers(&self, path: &Path) -> bool {
        self.files.contains(path) || self.trees.iter().any(|tree| path.starts_with(tree))
    }
}

fn operation_paths(database: &Database, operation_id: &str) -> BTreeSet<PathBuf> {
    let detail = at_db::ops::operation_detail(database.connection(), operation_id)
        .expect("operation detail")
        .expect("operation recorded");
    assert_eq!(
        detail.operation.kind,
        Some(at_db::ops::OperationKind::Migrate)
    );
    detail
        .files
        .iter()
        .filter_map(|file| file.path.as_ref().map(PathBuf::from))
        .collect()
}

#[test]
fn dry_run_rows_equal_applied_operation_files_for_every_strategy() -> Result<(), Box<dyn Error>> {
    for strategy in [
        MigrationStrategy::LeaveAndWatch,
        MigrationStrategy::ReplaceWithLinks,
    ] {
        let fixture = fixture();
        let plan = plan(&fixture, strategy);
        let rows = dry_run(&plan);
        let expected = planned(&rows);
        assert!(!expected.files.is_empty(), "the fixture writes something");

        // The dry run itself writes nothing.
        assert!(
            !fixture.store.join("skills/sample-skill").exists(),
            "dry run must not create the store item"
        );
        assert_eq!(
            fs::read(fixture.root.join("native/team-rules.md"))?,
            b"# Team rules\nbe kind\n",
            "dry run must not touch native files"
        );

        let mut database = Database::open_in_memory()?;
        let roots = ApplyRoots::new([fixture.root.clone()])?;
        let mut context = ApplyPlanContext {
            store_root: &fixture.store,
            backup_root: &fixture.backups,
            authorized_roots: &roots,
            database: &mut database,
            project_id: None,
        };
        let applied = apply_plan(&plan, &mut context)?;
        let actual = operation_paths(&database, &applied.operation_id);
        for path in &actual {
            assert!(
                expected.covers(path),
                "operation_files row `{}` is not named by the dry run for {strategy:?}",
                path.display()
            );
        }
        for path in &expected.files {
            assert!(
                actual.contains(path),
                "planned write `{}` has no operation_files row for {strategy:?}",
                path.display()
            );
        }
        for tree in &expected.trees {
            // A created tree is recorded either as the directory itself (one CopyTree row) or
            // as one row per file it contains.
            if actual.contains(tree) {
                assert!(tree.is_dir(), "created tree `{}` exists", tree.display());
                continue;
            }
            let mut walked = vec![tree.clone()];
            let mut found = false;
            while let Some(directory) = walked.pop() {
                for entry in fs::read_dir(&directory)? {
                    let entry = entry?;
                    let path = entry.path();
                    if entry.file_type()?.is_dir() {
                        walked.push(path);
                    } else {
                        found = true;
                        assert!(
                            actual.contains(&path),
                            "file `{}` under a created tree has no operation_files row",
                            path.display()
                        );
                    }
                }
            }
            assert!(found, "created tree `{}` is empty", tree.display());
        }

        // Every planned destination really exists (or, for replaced originals, is a link).
        for row in &rows {
            match row.action {
                MigrationAction::CreateStoreItem
                | MigrationAction::UpdateManifest
                | MigrationAction::RenderMcp
                | MigrationAction::RenderRule => {
                    let destination = row.destination.as_ref().expect("destination");
                    assert!(destination.exists(), "{} exists", destination.display());
                }
                MigrationAction::WriteLink => {
                    let destination = row.destination.as_ref().expect("destination");
                    assert!(
                        fs::symlink_metadata(destination)?.file_type().is_symlink(),
                        "{} is a link",
                        destination.display()
                    );
                }
                _ => {}
            }
        }
        let touched = applied
            .touched_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for destination in rows.iter().filter_map(|row| row.destination.clone()) {
            assert!(
                touched.contains(&destination),
                "reported touch list is missing `{}`",
                destination.display()
            );
        }
    }
    Ok(())
}

#[test]
fn a_second_apply_of_the_same_plan_is_refused_because_the_store_changed()
-> Result<(), Box<dyn Error>> {
    let fixture = fixture();
    let plan = plan(&fixture, MigrationStrategy::LeaveAndWatch);
    let mut database = Database::open_in_memory()?;
    let roots = ApplyRoots::new([fixture.root.clone()])?;
    let mut context = ApplyPlanContext {
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    apply_plan(&plan, &mut context)?;
    // The plan hash still matches, but its destinations are now occupied: the reviewed plan
    // must be rebuilt rather than silently replayed.
    let mut second = ApplyPlanContext {
        store_root: &fixture.store,
        backup_root: &fixture.backups,
        authorized_roots: &roots,
        database: &mut database,
        project_id: None,
    };
    let error = apply_plan(&plan, &mut second).expect_err("second apply refused");
    let text = error.to_string();
    assert!(
        text.contains("stale") || text.contains("expected") || text.contains("Concurrent"),
        "unexpected refusal: {text}"
    );
    Ok(())
}
