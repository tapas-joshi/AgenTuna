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

/// Scans adapter-declared rule paths for the selected agents and scopes.
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
                            let source =
                                sources.entry((*scope, path.clone())).or_insert_with(|| {
                                    RuleSource {
                                        scope: *scope,
                                        agent_consumers: Vec::new(),
                                        path: path.clone(),
                                        managed: contains_managed_marker(&bytes),
                                        sha256: at_fs::sha256_hex(&bytes),
                                        bytes,
                                        mtime,
                                        link_target,
                                    }
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
    let text = String::from_utf8_lossy(bytes);
    text.contains("<!-- agentuna:begin -->") && text.contains("<!-- agentuna:end -->")
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
}
