//! TA-028: Cursor adapter golden behaviour for the two cases the inline unit tests do not pin.
//!
//! AC2 drives every adapter operation against a machine with no Cursor installation and asserts
//! that none of them plans or performs a write. AC3 reads an MCP document through the registry's
//! TM-008 delegation seam, counts exactly one delegated call and hashes the fixture before and
//! after to prove the read left it byte-identical.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use at_adapters::adapter::{
    AgentAdapter, AgentAdapterRegistry, AgentDeployment, AgentEnvironment, AgentError,
    AgentIoDocument, DetectionState, McpAdapterDelegate,
};
use at_adapters::cursor_adapter::CursorAdapter;
use at_fs::sha256_hex;
use at_model::{AgentId, ItemKind, Scope};
use tempfile::TempDir;

/// A throwaway home and project, resolved once so compared paths are spelled the same way.
struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temporary directory");
        let root = fs::canonicalize(dir.path()).expect("canonical fixture root");
        let home = root.join("home");
        let project = root.join("project");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&project).expect("project");
        Self {
            _dir: dir,
            root,
            home,
            project,
        }
    }

    fn env(&self) -> AgentEnvironment {
        AgentEnvironment {
            home: self.home.clone(),
            project: Some(self.project.clone()),
            variables: BTreeMap::new(),
        }
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.home.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(&path, content).expect("fixture file");
        path
    }
}

/// Every file below `root` with its bytes hashed, so any write shows up.
fn snapshot(root: &Path) -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, String)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("read fixture directory")
            .map(|entry| entry.expect("directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push((
                    path.clone(),
                    sha256_hex(&fs::read(&path).expect("fixture bytes")),
                ));
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found
}

#[test]
fn not_installed_cursor_plans_no_writes_for_every_operation() {
    let fixture = Fixture::new();
    let adapter = CursorAdapter;
    let env = fixture.env();
    let before = snapshot(&fixture.root);
    assert_eq!(
        adapter.detect(&env).state,
        DetectionState::NotInstalled,
        "an empty home has no Cursor installation"
    );

    for kind in [ItemKind::Skill, ItemKind::McpServer, ItemKind::Rule] {
        let planned = adapter.ensure_dirs(kind, Scope::User, &env);
        assert!(
            matches!(
                &planned,
                Err(AgentError::NotInstalled { agent: AgentId::Cursor, path })
                    if path == &fixture.home.join(".cursor/skills")
            ),
            "{kind} must not plan directory writes for a missing agent: {planned:?}"
        );
    }

    // No adapter operation returns a reviewed ChangeSet for a missing agent: the settings
    // plan is unsupported for Cursor, so the only operations it can produce are directories.
    let settings =
        adapter.plan_settings_server_enabled(&env, "/project/.mcp.json", "demo", true, 1);
    assert!(
        matches!(
            &settings,
            Err(AgentError::Unsupported {
                agent: AgentId::Cursor,
                ..
            })
        ),
        "settings planning produces no operations for Cursor: {settings:?}"
    );

    assert!(
        adapter
            .list_native(&env)
            .expect("native listing")
            .is_empty(),
        "a missing agent lists no native items"
    );
    let discovery = adapter.discover(&env).expect("discovery");
    assert!(
        discovery.items.is_empty(),
        "discovery lists nothing: {:?}",
        discovery.items
    );
    assert!(
        discovery.diagnostics.iter().all(|diagnostic| matches!(
            diagnostic,
            at_adapters::cursor_adapter::CursorDiagnostic::FileMissing { .. }
        )),
        "missing roots are diagnostics, not writes"
    );
    assert!(
        matches!(
            adapter.read_io(ItemKind::McpServer, Scope::User, &env),
            Err(AgentError::Delegate { .. })
        ),
        "MCP reads are delegated, so the adapter itself reads nothing"
    );

    assert_eq!(
        snapshot(&fixture.root),
        before,
        "every attempted operation returned before touching the disk"
    );
    assert!(
        !fixture.home.join(".cursor").exists(),
        "nothing created the config root, so the agent stays NotInstalled"
    );
}

/// Records delegated MCP reads and replays the resolved Cursor document.
#[derive(Default)]
struct RecordingCursorMcpDelegate {
    reads: AtomicUsize,
}

impl McpAdapterDelegate for RecordingCursorMcpDelegate {
    fn read_io(&self, scope: Scope, env: &AgentEnvironment) -> Result<AgentIoDocument, AgentError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let candidate = CursorAdapter
            .paths(ItemKind::McpServer, scope, env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .ok_or_else(|| AgentError::Unsupported {
                agent: AgentId::Cursor,
                kind: String::from("MCP configuration"),
            })?;
        let content = fs::read(&candidate.path).map_err(|source| AgentError::Io {
            path: candidate.path.clone(),
            source,
        })?;
        Ok(AgentIoDocument {
            path: candidate.path,
            content,
        })
    }

    fn render_io(&self, _: &AgentDeployment) -> Result<Vec<u8>, AgentError> {
        Err(AgentError::Unsupported {
            agent: AgentId::Cursor,
            kind: String::from("MCP render is not exercised by a read"),
        })
    }

    fn write_io(&self, _: &AgentDeployment) -> Result<AgentIoDocument, AgentError> {
        Err(AgentError::Unsupported {
            agent: AgentId::Cursor,
            kind: String::from("MCP write is not exercised by a read"),
        })
    }
}

#[test]
fn mcp_read_delegates_once_and_leaves_the_fixture_unchanged() {
    let fixture = Fixture::new();
    let mcp_path = fixture.write(
        ".cursor/mcp.json",
        "{\"mcpServers\":{\"demo\":{\"command\":\"x\"}}}\n",
    );
    let env = fixture.env();
    let before = snapshot(&fixture.root);

    let registry = AgentAdapterRegistry::register();
    let delegate = Arc::new(RecordingCursorMcpDelegate::default());
    registry
        .register_mcp_delegate(AgentId::Cursor, delegate.clone())
        .expect("register the TM-008 delegate");

    let document = registry
        .read_io(&AgentId::Cursor, ItemKind::McpServer, Scope::User, &env)
        .expect("delegated MCP read");
    assert_eq!(
        delegate.reads.load(Ordering::SeqCst),
        1,
        "the document comes from exactly one TM-008 delegation call"
    );
    assert_eq!(document.path, mcp_path, "the resolved cursor path is used");
    assert_eq!(
        document.content,
        fs::read(&mcp_path).expect("fixture bytes"),
        "the delegated read returns the native bytes unchanged"
    );
    assert_eq!(
        snapshot(&fixture.root),
        before,
        "reading an MCP document leaves the fixture hash unchanged"
    );
}
