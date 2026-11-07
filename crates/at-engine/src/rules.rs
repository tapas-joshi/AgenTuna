//! Adapter-driven discovery of instruction files across scopes.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{AgentId, ItemKind, Scope};

/// One rule file discovered through native adapter paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSource {
    /// Scope assigned by the adapter path candidate.
    pub scope: Scope,
    /// Agents whose paths identify this file.
    pub agent_consumers: Vec<AgentId>,
    /// On-disk file path.
    pub path: PathBuf,
    /// Exact file bytes.
    pub bytes: Vec<u8>,
    /// Modification time when available.
    pub mtime: Option<SystemTime>,
    /// Lowercase SHA-256 of exact bytes.
    pub sha256: String,
    /// Whether the file contains AgenTuna's managed-block marker.
    pub managed: bool,
    /// Symlink destination when this path is a symlink.
    pub link_target: Option<PathBuf>,
}

/// One reviewed rule-render target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRenderTarget {
    /// Absolute destination path.
    pub path: PathBuf,
    /// Exact existing bytes, or `None` when the target does not exist.
    pub existing: Option<Vec<u8>>,
}

/// One target's textual change preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRenderDiff {
    /// Destination path.
    pub path: PathBuf,
    /// Unified diff from existing bytes to the rendered output.
    pub text: String,
}

/// Paired CLAUDE.md and AGENTS.md writes for one canonical rule source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRenderPlan {
    /// Reviewed multi-file operation, absent when both rendered files are already current.
    pub change_set: Option<at_model::ChangeSet>,
    /// Diffs for changed files only.
    pub diffs: Vec<RuleRenderDiff>,
}

/// Failures while planning a paired rule render.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuleRenderError {
    /// Managed-block ownership or markers were invalid.
    #[error(transparent)]
    ManagedBlock(#[from] ManagedBlockError),
    /// ChangeSet construction failed.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// A rule file is not valid UTF-8; it is never rewritten lossily.
    #[error("rule file at {path} is not valid UTF-8")]
    Parse {
        /// File that could not be decoded.
        path: PathBuf,
    },
}

/// Plans both native rule targets from one source without performing filesystem writes.
///
/// Apply the returned `ChangeSet` once through `at_fs::apply_with_roots` so both files
/// are journaled and rolled back as one operation.
///
/// # Errors
/// Returns [`RuleRenderError::ManagedBlock`] when either existing file requires adoption or
/// has malformed markers, [`RuleRenderError::Parse`] when a target or the rendered block is not
/// valid UTF-8 (such a file is never rewritten lossily), and [`RuleRenderError::ChangeSet`] for
/// invalid operations.
pub fn plan_rule_render(
    source: &[u8],
    targets: [RuleRenderTarget; 2],
    scope: Scope,
    project_id: Option<String>,
) -> Result<RuleRenderPlan, RuleRenderError> {
    use similar::TextDiff;
    let mut ops = Vec::with_capacity(2);
    let mut diffs = Vec::with_capacity(2);
    for target in targets {
        if !target.path.is_absolute() {
            return Err(at_model::ChangeSetError::InvalidPath {
                path: target.path.display().to_string(),
                reason: "rule target must be absolute".to_owned(),
            }
            .into());
        }
        if let Some(existing) = target.existing.as_deref() {
            std::str::from_utf8(existing).map_err(|_| RuleRenderError::Parse {
                path: target.path.clone(),
            })?;
        }
        let bytes = render_managed_block(&target.path, target.existing.as_deref(), source)?;
        let before = target.existing.as_deref().unwrap_or_default();
        if target.existing.as_deref() == Some(bytes.as_slice()) {
            continue;
        }
        let expected = match &target.existing {
            Some(existing) => at_model::ExpectedState::hash(at_fs::sha256_hex(existing))?,
            None => at_model::ExpectedState::absent(),
        };
        let before_text = std::str::from_utf8(before).map_err(|_| RuleRenderError::Parse {
            path: target.path.clone(),
        })?;
        let after_text = std::str::from_utf8(&bytes).map_err(|_| RuleRenderError::Parse {
            path: target.path.clone(),
        })?;
        let diff = TextDiff::from_lines(before_text, after_text)
            .unified_diff()
            .to_string();
        diffs.push(RuleRenderDiff {
            path: target.path.clone(),
            text: diff,
        });
        ops.push(at_model::FileOp::Write {
            path: target.path,
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
    }
    if ops.is_empty() {
        return Ok(RuleRenderPlan {
            change_set: None,
            diffs,
        });
    }
    let id = uuid::Uuid::now_v7().to_string();
    let change_set = at_model::ChangeSet {
        id: id.clone(),
        preview_id: id,
        operation_kind: at_model::OperationKind::parse("edit_rule")?,
        scope: Some(scope),
        project_id,
        summary: "Render shared instruction rules".to_owned(),
        ops,
        created_at_unix_seconds: 0,
    };
    change_set.validate()?;
    Ok(RuleRenderPlan {
        change_set: Some(change_set),
        diffs,
    })
}
/// Applies a reviewed two-file rule plan and records both written versions.
///
/// # Errors
/// Returns filesystem, operation-history, or database errors. Absolute target paths are accepted
/// only beneath `roots`; callers must supply the exact roots that were authorized for review.
pub fn apply_rule_render_plan(
    plan: &RuleRenderPlan,
    item_id: &at_model::ItemId,
    connection: &rusqlite::Connection,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
) -> Result<Option<at_fs::ApplyOutcome>, RuleRenderApplyError> {
    let Some(change_set) = &plan.change_set else {
        return Ok(None);
    };
    let outcome = at_fs::apply_with_roots(change_set, context, roots)?;
    for op in &change_set.ops {
        let at_model::FileOp::Write { path, bytes, .. } = op else {
            continue;
        };
        let hash = at_fs::sha256_hex(bytes);
        let size = i64::try_from(bytes.len()).map_err(|_| RuleRenderApplyError::VersionSize)?;
        let path = path.to_string_lossy();
        let mut version = at_db::versions::NewVersion::new(
            item_id,
            change_set.scope.unwrap_or(Scope::Project),
            &hash,
        )
        .with_path(&path)
        .with_size_mtime(size, 0)
        .with_operation(&outcome.operation_id);
        version.mtime = std::fs::metadata(path.as_ref())
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_millis()).ok());
        at_db::versions::record_skill_version(connection, &version)?;
    }
    Ok(Some(outcome))
}

/// Apply failures for a managed rule render.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RuleRenderApplyError {
    /// A safe-write or operation-history failure.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
    /// Version history could not be recorded.
    #[error(transparent)]
    Database(#[from] at_db::DbError),
    /// Rendered file size exceeds SQLite's signed integer range.
    #[error("rule file is too large to record in version history")]
    VersionSize,
}

/// A non-fatal issue encountered while discovering rule files.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuleDiagnostic {
    /// A symlink has no target.
    BrokenSymlink {
        /// Dangling symbolic-link path.
        path: PathBuf,
    },
    /// A rule file could not be read due to permissions.
    PermissionDenied {
        /// Unreadable path.
        path: PathBuf,
    },
    /// A candidate could not be inspected for another IO reason.
    Io {
        /// Path that failed.
        path: PathBuf,
        /// OS error description.
        message: String,
    },
    /// An adapter does not provide rule paths.
    Unsupported {
        /// Adapter identity.
        agent: AgentId,
    },
}

/// Stable discovery output and non-fatal diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleScan {
    /// Sorted rule files, deduplicated by scope and path.
    pub sources: Vec<RuleSource>,
    /// Sorted diagnostics.
    pub diagnostics: Vec<RuleDiagnostic>,
}
pub use at_fs::managed_block::{
    MANAGED_BEGIN, MANAGED_END, ManagedBlockError, render_managed_block,
};

/// A rule file in an agent's effective instruction composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedRule {
    /// Source rule metadata.
    pub source: RuleSource,
    /// Local Claude instructions add to and override earlier instructions; they do not replace.
    pub override_source: bool,
}

/// Compose scanned rules in stable native load order up to and including `scope`.
pub fn compose(
    agent: &AgentId,
    scope: Scope,
    scan: &RuleScan,
) -> (Vec<ComposedRule>, Vec<RuleDiagnostic>) {
    let supported = matches!(
        agent,
        AgentId::ClaudeCode | AgentId::Codex | AgentId::Omp | AgentId::Cursor
    ) || (matches!(agent, AgentId::Custom(_))
        && scan
            .sources
            .iter()
            .any(|source| source.agent_consumers.contains(agent)));
    if !supported {
        return (
            Vec::new(),
            vec![RuleDiagnostic::Unsupported {
                agent: agent.clone(),
            }],
        );
    }
    let mut sources: Vec<_> = scan
        .sources
        .iter()
        .filter(|source| {
            (source.scope.precedence() <= scope.precedence()
                || (scope == Scope::Project && source.scope == Scope::Local))
                && source.agent_consumers.contains(agent)
                && match agent {
                    AgentId::ClaudeCode => source.path.file_name().is_some_and(|n| {
                        n.eq_ignore_ascii_case("claude.md")
                            || n.eq_ignore_ascii_case("claude.local.md")
                    }),
                    AgentId::Codex | AgentId::Omp => source
                        .path
                        .file_name()
                        .is_some_and(|n| n.eq_ignore_ascii_case("agents.md")),
                    AgentId::Cursor => source
                        .path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("mdc")),
                    AgentId::Custom(_) => true,
                }
        })
        .cloned()
        .collect();
    sources.sort_by(|a, b| {
        (a.scope.precedence(), a.path.file_name(), &a.path).cmp(&(
            b.scope.precedence(),
            b.path.file_name(),
            &b.path,
        ))
    });
    let composed = sources
        .into_iter()
        .map(|source| ComposedRule {
            override_source: agent == &AgentId::ClaudeCode
                && source.scope == Scope::Local
                && source
                    .path
                    .file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("claude.local.md")),
            source,
        })
        .collect();
    (composed, Vec::new())
}

/// Scans the supported adapters for the requested scope using injected roots.
///
/// # Examples
/// ```no_run
/// # use std::path::Path;
/// # use at_model::Scope;
/// let scan = at_engine::rules::scan_registered(Path::new("/home/user"), None, Scope::User);
/// let _ = scan.sources;
/// ```
pub fn scan_registered(home: &Path, project: Option<&Path>, scope: Scope) -> RuleScan {
    let registry = AgentAdapterRegistry::register();
    let env = AgentEnvironment {
        home: home.to_path_buf(),
        project: project.map(Path::to_path_buf),
        variables: Default::default(),
    };
    let agents = [
        AgentId::ClaudeCode,
        AgentId::Codex,
        AgentId::Omp,
        AgentId::Cursor,
    ];
    scan(&registry, &env, &agents, &[scope])
}

/// Scans adapter-declared rule paths for selected agents and scopes.
///
/// Duplicate paths are merged into one source with all consuming agents. Results are stable
/// regardless of adapter enumeration order.
pub fn scan(
    registry: &AgentAdapterRegistry,
    env: &AgentEnvironment,
    agents: &[AgentId],
    scopes: &[Scope],
) -> RuleScan {
    let mut sources = std::collections::BTreeMap::<(Scope, PathBuf), RuleSource>::new();
    let mut diagnostics = Vec::new();
    let mut diagnostic_keys = BTreeSet::new();
    for agent in agents {
        let adapter = match registry.get(agent) {
            Ok(Some(adapter)) => adapter,
            Ok(None) => {
                diagnostics.push(RuleDiagnostic::Unsupported {
                    agent: agent.clone(),
                });
                continue;
            }
            Err(error) => {
                diagnostics.push(RuleDiagnostic::Io {
                    path: PathBuf::new(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        for scope in scopes {
            let candidates = adapter.paths(ItemKind::Rule, *scope, env);
            if candidates.is_empty() {
                diagnostics.push(RuleDiagnostic::Unsupported {
                    agent: agent.clone(),
                });
            }
            for candidate in candidates {
                let mut paths = Vec::new();
                if candidate.kind == "rules_dir" {
                    match fs::read_dir(&candidate.path) {
                        Ok(entries) => {
                            for entry in entries.flatten() {
                                let path = entry.path();
                                if path
                                    .extension()
                                    .is_some_and(|ext| ext.eq_ignore_ascii_case("mdc"))
                                {
                                    paths.push(path);
                                }
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                        Err(e) => {
                            record_io(&mut diagnostics, &mut diagnostic_keys, candidate.path, e);
                            continue;
                        }
                    }
                } else {
                    paths
                        .push(case_insensitive_existing(&candidate.path).unwrap_or(candidate.path));
                }
                paths.sort();
                for path in paths {
                    match read_rule(&path) {
                        Ok((bytes, mtime, link_target)) => {
                            let resolved_scope =
                                if *scope == Scope::Global && path.starts_with(&env.home) {
                                    Scope::User
                                } else {
                                    *scope
                                };
                            let source = sources
                                .entry((resolved_scope, path.clone()))
                                .or_insert_with(|| RuleSource {
                                    scope: resolved_scope,
                                    agent_consumers: Vec::new(),
                                    path: path.clone(),
                                    managed: contains_managed_marker(&bytes),
                                    sha256: at_fs::sha256_hex(&bytes),
                                    bytes,
                                    mtime,
                                    link_target,
                                });
                            if !source.agent_consumers.contains(agent) {
                                source.agent_consumers.push(agent.clone());
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                            let key = (path.clone(), "permission".to_owned());
                            if diagnostic_keys.insert(key) {
                                diagnostics.push(RuleDiagnostic::PermissionDenied { path });
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                            let key = (path.clone(), "broken".to_owned());
                            if diagnostic_keys.insert(key) {
                                diagnostics.push(RuleDiagnostic::BrokenSymlink { path });
                            }
                        }
                        Err(e) => record_io(&mut diagnostics, &mut diagnostic_keys, path, e),
                    }
                }
            }
        }
    }
    let mut sources: Vec<_> = sources.into_values().collect();
    for source in &mut sources {
        source.agent_consumers.sort();
    }
    diagnostics.sort_by(|a, b| diagnostic_path(a).cmp(diagnostic_path(b)));
    RuleScan {
        sources,
        diagnostics,
    }
}

fn case_insensitive_existing(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    let entries: Vec<PathBuf> = fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries
        .iter()
        .find(|candidate| candidate.file_name().and_then(|s| s.to_str()) == Some(name))
        .cloned()
        .or_else(|| {
            entries.into_iter().find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(name))
            })
        })
}

fn read_rule(path: &Path) -> io::Result<(Vec<u8>, Option<SystemTime>, Option<PathBuf>)> {
    let metadata = fs::symlink_metadata(path)?;
    let link_target = if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if fs::metadata(path).is_err() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "broken symlink",
            ));
        }
        Some(target)
    } else {
        None
    };
    let bytes = fs::read(path)?;
    let modified = fs::metadata(path).ok().and_then(|m| m.modified().ok());
    Ok((bytes, modified, link_target))
}

fn contains_managed_marker(bytes: &[u8]) -> bool {
    // Markers are ASCII; invalid UTF-8 is never decoded lossily just to look for them.
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.contains("<!-- agentuna:begin -->") && text.contains("<!-- agentuna:end -->")
    })
}

fn record_io(
    out: &mut Vec<RuleDiagnostic>,
    keys: &mut BTreeSet<(PathBuf, String)>,
    path: PathBuf,
    e: io::Error,
) {
    if keys.insert((path.clone(), e.to_string())) {
        out.push(RuleDiagnostic::Io {
            path,
            message: e.to_string(),
        });
    }
}
fn diagnostic_path(d: &RuleDiagnostic) -> &Path {
    match d {
        RuleDiagnostic::BrokenSymlink { path }
        | RuleDiagnostic::PermissionDenied { path }
        | RuleDiagnostic::Io { path, .. } => path,
        RuleDiagnostic::Unsupported { .. } => Path::new(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_adapters::adapter::AgentEnvironment;
    use std::collections::BTreeMap;

    #[test]
    fn managed_renderer_preserves_outside_bytes_and_requires_adoption() {
        let path = Path::new("CLAUDE.md");
        let original =
            b"top\r\n<!-- agentuna:begin -->\r\nold\r\n<!-- agentuna:end -->\r\nbottom\r\n";
        let rendered = render_managed_block(path, Some(original), b"new rule").expect("render");
        // The block body uses the file's own CRLF ending; the bytes around it are unchanged.
        assert_eq!(
            rendered,
            b"top\r\n<!-- agentuna:begin -->\r\nnew rule\r\n<!-- agentuna:end -->\r\nbottom\r\n"
        );
        assert_eq!(
            render_managed_block(path, Some(b"user authored\n"), b"new"),
            Err(ManagedBlockError::AdoptRequired {
                path: path.to_path_buf()
            })
        );
        assert!(matches!(
            render_managed_block(path, Some(b"<!-- agentuna:begin -->\nbody"), b"new"),
            Err(ManagedBlockError::MalformedManagedBlock { line: 1, .. })
        ));
    }

    #[test]
    fn rendering_after_external_outside_edit_preserves_the_new_external_bytes() {
        let path = Path::new("CLAUDE.md");
        let existing = b"external header\n<!-- agentuna:begin -->\nold\n<!-- agentuna:end -->\nexternal footer\n";
        let rendered = render_managed_block(path, Some(existing), b"updated").expect("render");
        assert_eq!(
            rendered,
            b"external header\n<!-- agentuna:begin -->\nupdated\n<!-- agentuna:end -->\nexternal footer\n"
        );
    }
    #[test]
    fn paired_rule_plan_writes_identical_blocks_and_noops_when_current() {
        let root = tempfile::tempdir().expect("root");
        let claude = root.path().join("CLAUDE.md");
        let agents = root.path().join("AGENTS.md");
        let targets = [
            RuleRenderTarget {
                path: claude.clone(),
                existing: None,
            },
            RuleRenderTarget {
                path: agents.clone(),
                existing: None,
            },
        ];
        let plan = plan_rule_render(b"# shared\n", targets, Scope::Project, Some("repo".into()))
            .expect("plan");
        let set = plan.change_set.expect("two-file write");
        assert_eq!(set.ops.len(), 2);
        let rendered = match &set.ops[0] {
            at_model::FileOp::Write { bytes, .. } => bytes,
            _ => panic!("expected file writes"),
        };
        match &set.ops[1] {
            at_model::FileOp::Write { bytes, .. } => assert_eq!(bytes, rendered),
            _ => panic!("expected file writes"),
        }
        let current = Some(rendered.clone());
        let plan = plan_rule_render(
            b"# shared\n",
            [
                RuleRenderTarget {
                    path: claude,
                    existing: current.clone(),
                },
                RuleRenderTarget {
                    path: agents,
                    existing: current,
                },
            ],
            Scope::Project,
            Some("repo".into()),
        )
        .expect("idempotent plan");
        assert!(plan.change_set.is_none());
        assert!(plan.diffs.is_empty());
    }
    #[test]
    fn composition_includes_local_override_after_project() {
        let source = |scope, name: &str| RuleSource {
            scope,
            agent_consumers: vec![AgentId::ClaudeCode],
            path: PathBuf::from(name),
            bytes: Vec::new(),
            mtime: None,
            sha256: String::new(),
            managed: false,
            link_target: None,
        };
        let scan = RuleScan {
            sources: vec![
                source(Scope::Local, "CLAUDE.local.md"),
                source(Scope::Project, "CLAUDE.md"),
                source(Scope::User, "CLAUDE.md"),
                source(Scope::Global, "CLAUDE.md"),
            ],
            diagnostics: Vec::new(),
        };
        let (composed, diagnostics) = compose(&AgentId::ClaudeCode, Scope::Project, &scan);
        assert!(diagnostics.is_empty());
        assert_eq!(
            composed
                .iter()
                .map(|rule| rule.source.scope)
                .collect::<Vec<_>>(),
            vec![Scope::Global, Scope::User, Scope::Project, Scope::Local]
        );
        assert!(composed[3].override_source);
    }

    #[test]
    fn claude_and_codex_instruction_composition_has_stable_native_order() {
        let rule = |scope, agent, name: &str| RuleSource {
            scope,
            agent_consumers: vec![agent],
            path: PathBuf::from(name),
            bytes: Vec::new(),
            mtime: None,
            sha256: String::new(),
            managed: false,
            link_target: None,
        };
        let scan = RuleScan {
            sources: vec![
                rule(Scope::Local, AgentId::ClaudeCode, "CLAUDE.local.md"),
                rule(Scope::Project, AgentId::ClaudeCode, "CLAUDE.md"),
                rule(Scope::User, AgentId::ClaudeCode, "CLAUDE.md"),
                rule(Scope::Global, AgentId::ClaudeCode, "CLAUDE.md"),
                rule(Scope::Project, AgentId::Codex, "AGENTS.md"),
                rule(Scope::User, AgentId::Codex, "AGENTS.md"),
                rule(Scope::User, AgentId::Codex, "CLAUDE.md"),
            ],
            diagnostics: Vec::new(),
        };
        let (claude, _) = compose(&AgentId::ClaudeCode, Scope::Project, &scan);
        assert_eq!(
            claude
                .iter()
                .map(|source| source.source.scope)
                .collect::<Vec<_>>(),
            vec![Scope::Global, Scope::User, Scope::Project, Scope::Local]
        );
        let (codex, _) = compose(&AgentId::Codex, Scope::Project, &scan);
        assert_eq!(
            codex
                .iter()
                .map(|source| source.source.scope)
                .collect::<Vec<_>>(),
            vec![Scope::User, Scope::Project]
        );
        assert!(codex.iter().all(|source| {
            source
                .source
                .path
                .file_name()
                .is_some_and(|name| name == "AGENTS.md")
        }));
    }
    #[test]
    fn discovers_project_rule_and_merges_consumers_deterministically() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("CLAUDE.md");
        fs::write(&path, "# instructions\n").expect("write fixture");
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: Some(root.path().to_path_buf()),
            variables: BTreeMap::new(),
        };
        let scan = scan(&registry, &env, &[AgentId::ClaudeCode], &[Scope::Project]);
        assert_eq!(
            scan.sources.len(),
            1,
            "{:?}",
            scan.sources.iter().map(|s| &s.path).collect::<Vec<_>>()
        );
        assert_eq!(scan.sources[0].scope, Scope::Project);
        assert_eq!(scan.sources[0].agent_consumers, vec![AgentId::ClaudeCode]);
        assert_eq!(scan.sources[0].path, path);
        assert_eq!(
            scan.sources[0].sha256,
            at_fs::sha256_hex(b"# instructions\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_broken_symlinks_without_aborting_scan() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().expect("tempdir");
        symlink(root.path().join("missing"), root.path().join("CLAUDE.md")).expect("symlink");
        fs::write(root.path().join("CLAUDE.local.md"), "# other\n").expect("write rule");
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: Some(root.path().to_path_buf()),
            variables: BTreeMap::new(),
        };
        let scan = scan(&registry, &env, &[AgentId::ClaudeCode], &[Scope::Project]);
        assert!(
            scan.sources
                .iter()
                .any(|source| source.path.ends_with("CLAUDE.local.md"))
        );
        assert!(scan.diagnostics.iter().any(
            |d| matches!(d, RuleDiagnostic::BrokenSymlink { path } if path.ends_with("CLAUDE.md"))
        ));
    }

    #[test]
    fn lowercase_on_disk_claude_md_is_discovered_and_composed() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("claude.md");
        fs::write(&path, "# lowercase instructions\n").expect("write fixture");
        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: Some(root.path().to_path_buf()),
            variables: BTreeMap::new(),
        };
        let scan = scan(&registry, &env, &[AgentId::ClaudeCode], &[Scope::Project]);
        assert_eq!(
            scan.sources.len(),
            1,
            "{:?}",
            scan.sources.iter().map(|s| &s.path).collect::<Vec<_>>()
        );
        assert_eq!(
            scan.sources[0].path, path,
            "the candidate spelling is replaced by the actual on-disk name"
        );
        assert_eq!(
            scan.sources[0]
                .path
                .file_name()
                .and_then(|name| name.to_str()),
            Some("claude.md")
        );
        assert_eq!(
            scan.sources[0].sha256,
            at_fs::sha256_hex(b"# lowercase instructions\n")
        );

        // Composition matches the filename without regard to case, so the lowercase file
        // reaches Claude Code's instruction chain.
        let (composed, diagnostics) = compose(&AgentId::ClaudeCode, Scope::Project, &scan);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(composed.len(), 1);
        assert_eq!(composed[0].source.path, path);
        assert_eq!(composed[0].source.scope, Scope::Project);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_rule_is_reported_while_other_rules_are_still_returned() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("tempdir");
        let denied = root.path().join("CLAUDE.md");
        let readable = root.path().join("CLAUDE.local.md");
        fs::write(&denied, "# unreadable\n").expect("write denied fixture");
        fs::write(&readable, "# readable\n").expect("write readable fixture");
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        if fs::read(&denied).is_ok() {
            // The test process bypasses permission bits (root in a container): the
            // PermissionDenied branch is unreachable for it, not silently passing.
            return;
        }

        let registry = AgentAdapterRegistry::register();
        let env = AgentEnvironment {
            home: root.path().join("home"),
            project: Some(root.path().to_path_buf()),
            variables: BTreeMap::new(),
        };
        let scan = scan(&registry, &env, &[AgentId::ClaudeCode], &[Scope::Project]);
        assert!(
            scan.sources.iter().any(|source| source.path == readable),
            "readable rules are still returned: {:?}",
            scan.sources.iter().map(|s| &s.path).collect::<Vec<_>>()
        );
        assert!(
            scan.sources.iter().all(|source| source.path != denied),
            "an unreadable file never becomes a RuleSource"
        );
        assert!(
            scan.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                RuleDiagnostic::PermissionDenied { path } if *path == denied
            )),
            "the unreadable path is reported: {:?}",
            scan.diagnostics
        );
    }
}
