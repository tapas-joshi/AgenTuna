//! Object-safe agent adapter contract and shared in-process registry.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use at_model::{AgentId, ItemKind, Scope};

use crate::mcp_capabilities::{AgentMcpCapabilities, capabilities};

/// Injected machine context used by pure agent detection and path discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEnvironment {
    /// Home directory used instead of consulting process-global state.
    pub home: PathBuf,
    /// Project root for Project and Local scope paths, when available.
    pub project: Option<PathBuf>,
    /// Environment variables supplied by the caller.
    pub variables: BTreeMap<String, String>,
}

/// Candidate location for one agent configuration surface.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AgentPathCandidate {
    /// Path kind, such as `skills_dir`, `mcp_file`, or `rules_file`.
    pub kind: String,
    /// Native candidate path.
    pub path: PathBuf,
    /// Scope represented by the path.
    pub scope: Scope,
    /// File format when the path is a file.
    pub format: Option<String>,
    /// Whether this is the adapter's preferred candidate.
    pub primary: bool,
}

/// Detection result without performing shell execution or filesystem mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DetectionState {
    /// Expected native config or skill root exists.
    Installed,
    /// Some related config exists, but the agent's main executable was not probed.
    ConfiguredOnly,
    /// No known native paths exist.
    NotInstalled,
}

/// Agent presence plus its ordered candidate paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDetection {
    /// Detection state.
    pub state: DetectionState,
    /// Candidate paths useful to the connect dialog even when missing.
    pub candidates: Vec<AgentPathCandidate>,
}

/// Detection result augmented with a version supplied by the launch-time probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDetectionWithVersion {
    /// Presence and candidate paths.
    pub presence: AgentDetection,
    /// Parsed semantic version, when the probe supplied one.
    pub version: Option<String>,
}

/// Parses the first `major.minor.patch` sequence in command output.
#[must_use]
pub fn parse_probe_version(output: &str) -> Option<String> {
    let bytes = output.as_bytes();
    for start in 0..bytes.len() {
        let prefixed_v = start > 0
            && matches!(bytes[start - 1], b'v' | b'V')
            && (start == 1 || !bytes[start - 2].is_ascii_alphanumeric());
        if !bytes[start].is_ascii_digit()
            || start > 0
                && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'.')
                && !prefixed_v
        {
            continue;
        }
        let mut end = start;
        let mut separators = 0;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            if bytes[end] == b'.' {
                separators += 1;
            }
            end += 1;
        }
        if separators == 2
            && bytes[start..end]
                .split(|byte| *byte == b'.')
                .all(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
            && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric())
        {
            return Some(String::from_utf8_lossy(&bytes[start..end]).into_owned());
        }
    }
    None
}

/// Adapter-reported capabilities outside the delegated MCP format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCapabilities {
    /// MCP format capabilities owned by the TM adapter layer, if supported.
    pub mcp: Option<AgentMcpCapabilities>,
    /// Whether this agent can consume symlinked skill deployments.
    pub supports_symlink: bool,
    /// Whether native Markdown rule files are supported.
    pub supports_rules: bool,
}

/// Whether a write to this agent's native configuration document may follow a final symlink.
///
/// Dotfile managers commonly symlink Claude Code's `~/.claude.json` to a managed dotfiles
/// repository. The agent reads the link, so updating the resolved target (and keeping the link)
/// is what the user sees in the running agent; hashing the link text instead would misread an
/// untouched file as edited. Every other agent keeps the safe default that refuses a symlinked
/// destination unless an adapter opts in with evidence.
///
/// Following is not unrestricted: at-fs validates that the resolved target stays inside the
/// operation's authorized roots, and an operation that does not allow following refuses the
/// link with [`at_fs::FsError::SymlinkRefused`] before any write.
#[must_use]
pub fn follows_native_config_symlink(agent: &AgentId) -> bool {
    matches!(agent, AgentId::ClaudeCode)
}

/// A native item found by an adapter's bounded, read-only enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeItem {
    /// Kind of item found.
    pub kind: ItemKind,
    /// Scope in which the native item is visible.
    pub scope: Scope,
    /// Display or native map name.
    pub name: String,
    /// Native item path (the skill folder or rule file).
    pub path: PathBuf,
}

/// Deployment payload supplied to a renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDeployment {
    /// Item kind.
    pub kind: ItemKind,
    /// Scope to render.
    pub scope: Scope,
    /// Destination path selected by verified path resolution.
    pub path: PathBuf,
    /// Canonical item bytes.
    pub content: Vec<u8>,
}

/// Opaque read result for an adapter-owned document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIoDocument {
    /// Source path read.
    pub path: PathBuf,
    /// Original bytes, unchanged.
    pub content: Vec<u8>,
}

/// Text-only failure returned by an MCP delegate without exposing server configuration.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AgentDelegateFailure {
    message: String,
}

impl AgentDelegateFailure {
    /// Creates a redacted delegate error message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Typed adapter failures; paths are absolute whenever the caller supplied an absolute root.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The agent has no usable installation at the requested path.
    #[error("agent `{agent}` is not installed or configured at {path}")]
    NotInstalled {
        /// Agent identity.
        agent: AgentId,
        /// Missing agent path.
        path: PathBuf,
    },
    /// A native configuration document could not be parsed.
    #[error("invalid agent configuration at {path} (line {line}, column {col})")]
    Parse {
        /// Document path.
        path: PathBuf,
        /// 1-based line.
        line: usize,
        /// 1-based column.
        col: usize,
    },
    /// A path exists but cannot be read.
    #[error("agent configuration is not readable at {path}")]
    Permission {
        /// Unreadable path.
        path: PathBuf,
    },
    /// Requested operation is not supported for this kind.
    #[error("agent `{agent}` does not support `{kind}`")]
    Unsupported {
        /// Agent that rejected the operation.
        agent: AgentId,
        /// Item kind or operation name that was unsupported.
        kind: String,
    },
    /// A configuration changed before an adapter operation could be applied.
    #[error("agent configuration changed at {path}")]
    Conflict {
        /// Conflicting document path.
        path: PathBuf,
    },
    /// MCP operation failed in its delegated TM adapter.
    #[error("MCP adapter delegate failed: {source}")]
    Delegate {
        /// Error reported by the TM-owned adapter.
        #[source]
        source: AgentDelegateFailure,
    },
    /// Rule file ownership or managed-block markers prevented a render.
    #[error(transparent)]
    RuleRender(#[from] at_fs::managed_block::ManagedBlockError),
    /// Filesystem access failed at an agent path.
    #[error("agent path {path}: {source}")]
    Io {
        /// Path where the operation failed.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// Registry lock was poisoned.
    #[error("agent registry lock is poisoned")]
    RegistryPoisoned,
}

/// One adapter's object-safe contract for discovery and native configuration.
pub trait AgentAdapter: Send + Sync {
    /// Stable identity served by this adapter.
    fn id(&self) -> AgentId;

    /// Returns adapter-specific support declarations.
    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities;

    /// Detects known configuration from an injected environment; does not shell out.
    fn detect(&self, env: &AgentEnvironment) -> AgentDetection;

    /// Returns an already-available version result. Implementations may return `None` when no
    /// cached version probe is available; synchronous detection must not spawn a process.
    fn probe_version(&self) -> Result<Option<String>, AgentError> {
        Ok(None)
    }
    /// Detects an agent and attaches the semantic version parsed from an injected probe result.
    ///
    /// The probe is performed by the detection orchestrator; adapters never spawn a process.
    #[must_use]
    fn detect_with_version(
        &self,
        env: &AgentEnvironment,
        probe_output: Option<&str>,
    ) -> AgentDetectionWithVersion {
        AgentDetectionWithVersion {
            presence: self.detect(env),
            version: probe_output.and_then(parse_probe_version),
        }
    }

    /// Whether this adapter declares a supported native MCP configuration format.
    #[must_use]
    fn supports_mcp(&self) -> bool {
        true
    }

    /// Returns ordered path candidates for a kind and scope.
    fn paths(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate>;

    /// Lists native skills and rules without following symlinks or mutating the filesystem.
    ///
    /// Results are bounded to 10,000 directory entries per adapter and stable-sorted by path.
    ///
    /// # Errors
    /// Returns [`AgentError::Permission`] or [`AgentError::Io`] when a candidate root cannot be
    /// inspected.
    fn list_native(&self, env: &AgentEnvironment) -> Result<Vec<NativeItem>, AgentError> {
        let id = self.id();
        list_native_items(&id, None, |kind, scope| self.paths(kind, scope, env))
    }
    /// Lists native skill folders with parse and link diagnostics.
    ///
    /// This is a read-only view; it never adopts or modifies a discovered skill.
    fn list_native_skills(
        &self,
        env: &AgentEnvironment,
        store_roots: &[PathBuf],
    ) -> Result<Vec<crate::native_skills::NativeSkill>, AgentError> {
        crate::native_skills::list_native_skills(
            &self.id(),
            env,
            store_roots,
            self.paths(ItemKind::Skill, Scope::User, env)
                .into_iter()
                .chain(self.paths(ItemKind::Skill, Scope::Project, env)),
        )
    }

    /// Lists native skill folders including links into an AgenTuna store.
    ///
    /// The migration scan uses this to label store deployments as Managed; the skills list
    /// uses [`Self::list_native_skills`], which omits them.
    ///
    /// # Errors
    /// Returns [`AgentError::Permission`] or [`AgentError::Io`] when a candidate root cannot
    /// be inspected.
    fn list_native_skills_including_managed(
        &self,
        env: &AgentEnvironment,
        store_roots: &[PathBuf],
    ) -> Result<Vec<crate::native_skills::NativeSkill>, AgentError> {
        crate::native_skills::list_native_skills_with_options(
            &self.id(),
            env,
            store_roots,
            self.paths(ItemKind::Skill, Scope::User, env)
                .into_iter()
                .chain(self.paths(ItemKind::Skill, Scope::Project, env)),
            true,
        )
    }

    /// Plans Claude project-settings MCP enablement as a ChangeSet.
    ///
    /// Non-Claude adapters return `Unsupported`; no filesystem operation is performed here.
    fn plan_settings_server_enabled(
        &self,
        _env: &AgentEnvironment,
        _source: &str,
        _server_name: &str,
        _enabled: bool,
        _created_at_unix_seconds: u64,
    ) -> Result<at_model::ChangeSet, AgentError> {
        Err(AgentError::Unsupported {
            agent: self.id(),
            kind: String::from("Claude project settings"),
        })
    }

    /// Plans one Local skill deployment and its idempotent Git exclude update.
    ///
    /// The selected skills root comes from this adapter's Local path table; returned operations
    /// must be reviewed and applied through at-fs.
    ///
    /// # Errors
    /// Returns a typed adapter error for invalid paths, existing targets or malformed excludes.
    fn plan_local_skill(
        &self,
        project: &Path,
        slug: &str,
        store_skill: &Path,
        exclude_contents: Option<&[u8]>,
        created_at_unix_seconds: u64,
    ) -> Result<at_model::ChangeSet, AgentError> {
        let env = AgentEnvironment {
            home: project.to_path_buf(),
            project: Some(project.to_path_buf()),
            variables: BTreeMap::new(),
        };
        // The caller gates this materialization on the agent's installation and management
        // state: a Local deployment targets the project, which exists regardless of the
        // agent's user-scope install, so detection cannot decide it here.
        let target = self.skill_target(Scope::Local, &env, slug)?;
        plan_local_skill_change(
            &self.id(),
            project,
            slug,
            store_skill,
            &target,
            exclude_contents,
            created_at_unix_seconds,
        )
    }

    /// Lists Codex project trust metadata without exposing other config values.
    fn list_project_trust(
        &self,
        _config_path: &Path,
        _source: &str,
    ) -> Result<Vec<crate::codex_projects::CodexProjectTrust>, AgentError> {
        Err(AgentError::Unsupported {
            agent: self.id(),
            kind: String::from("project trust metadata"),
        })
    }

    /// Returns safe directory-creation operations for the requested surface.
    ///
    /// The caller must review and apply these operations through at-fs; this method never writes.
    ///
    /// # Errors
    /// Returns [`AgentError::Unsupported`] when the adapter has no path for this surface.
    fn ensure_dirs(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Result<Vec<at_model::FileOp>, AgentError> {
        let candidates = self.paths(kind, scope, env);
        // Creating an agent's directory tree would make a missing agent look configured, so a
        // NotInstalled agent gets zero operations for every kind (TA-028 AC2).
        if self.detect(env).state == DetectionState::NotInstalled {
            return Err(AgentError::NotInstalled {
                agent: self.id(),
                path: missing_installation_path(self, env),
            });
        }
        if candidates.is_empty() {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: format!("{kind} directory"),
            });
        }
        let selected_skills_root = if kind == ItemKind::Skill {
            select_skill_root(&candidates)
        } else {
            None
        };
        let mut directories = BTreeSet::new();
        for candidate in candidates {
            let directory = if let Some(root) = &selected_skills_root {
                root.clone()
            } else if candidate.kind == "skills_dir" || candidate.kind == "rules_dir" {
                candidate.path
            } else {
                candidate
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or(candidate.path)
            };
            if !directory.exists() {
                directories.insert(directory);
            }
        }
        Ok(directories
            .into_iter()
            .map(|path| at_model::FileOp::Mkdir {
                path,
                mode: None,
                parents: true,
            })
            .collect())
    }

    /// Resolves the target directory for one named skill from this agent's native path table.
    ///
    /// # Errors
    /// Returns [`AgentError::Unsupported`] when this agent has no skills root for the scope.
    fn skill_target(
        &self,
        scope: Scope,
        env: &AgentEnvironment,
        slug: &str,
    ) -> Result<PathBuf, AgentError> {
        if slug.is_empty() || slug == "." || slug == ".." || slug.contains(['/', '\\']) {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: String::from("invalid skill slug"),
            });
        }
        let candidates = self.paths(ItemKind::Skill, scope, env);
        select_skill_root(&candidates)
            .map(|root| root.join(slug))
            .ok_or_else(|| AgentError::Unsupported {
                agent: self.id(),
                kind: format!("skills directory for {scope}"),
            })
    }

    /// Deploys one canonical skill using the shared guarded at-fs strategy.
    ///
    /// Callers must check installation and obtain review/authorization before invoking this
    /// materialization primitive. Local-scope callers also plan the corresponding Git exclude.
    ///
    /// # Errors
    /// Returns typed filesystem failures, including unmanaged target conflicts.
    fn deploy_skill(
        &self,
        source: &Path,
        target: &Path,
        strategy: at_fs::SkillStrategy,
    ) -> Result<at_fs::SkillDeployOutcome, at_fs::FsError> {
        at_fs::deploy_skill(&at_fs::SkillDeployRequest::new(source, target, strategy))
    }

    /// Removes a deployment only when its recorded ownership matches the request.
    ///
    /// # Errors
    /// Returns typed filesystem failures; canonical store content is never removed.
    fn undeploy_skill(
        &self,
        request: &at_fs::RemoveRequest,
    ) -> Result<at_fs::RemoveOutcome, at_fs::FsError> {
        at_fs::remove_deployment(request)
    }

    /// Reads an adapter-owned skills index or rules document. MCP is delegated to TM adapters.
    ///
    /// # Errors
    /// Returns a typed error for unsupported kinds, missing paths, or filesystem failures.
    fn read_io(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Result<AgentIoDocument, AgentError> {
        read_io_default(self, kind, scope, env)
    }

    /// Renders deployment bytes; rule files are wrapped in an AgenTuna-managed block.
    ///
    /// # Errors
    /// Returns [`AgentError::Delegate`] for MCP rendering, [`AgentError::Parse`] when an existing
    /// rule file is not valid UTF-8 (it is refused instead of being rewritten lossily), and a
    /// typed adapter error when a managed rule block cannot be rendered.
    fn render_io(&self, deployment: &AgentDeployment) -> Result<Vec<u8>, AgentError> {
        if deployment.kind == ItemKind::McpServer {
            return Err(AgentError::Delegate {
                source: AgentDelegateFailure::new("MCP rendering is delegated to TM adapters"),
            });
        }
        if deployment.kind == ItemKind::Rule {
            let existing = match std::fs::symlink_metadata(&deployment.path) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    if metadata.permissions().readonly() {
                        return Err(AgentError::Permission {
                            path: deployment.path.clone(),
                        });
                    }
                    Some(std::fs::read(&deployment.path).map_err(|source| {
                        if source.kind() == std::io::ErrorKind::PermissionDenied {
                            AgentError::Permission {
                                path: deployment.path.clone(),
                            }
                        } else {
                            AgentError::Io {
                                path: deployment.path.clone(),
                                source,
                            }
                        }
                    })?)
                }
                Ok(_) => {
                    return Err(AgentError::Unsupported {
                        agent: self.id(),
                        kind: format!("non-regular rule path {}", deployment.path.display()),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    return Err(AgentError::Permission {
                        path: deployment.path.clone(),
                    });
                }
                Err(source) => {
                    return Err(AgentError::Io {
                        path: deployment.path.clone(),
                        source,
                    });
                }
            };
            if let Some(existing) = existing.as_deref()
                && let Err(error) = std::str::from_utf8(existing)
            {
                let (line, col) = utf8_line_col(existing, error.valid_up_to());
                return Err(AgentError::Parse {
                    path: deployment.path.clone(),
                    line,
                    col,
                });
            }
            return Ok(at_fs::managed_block::render_managed_block(
                &deployment.path,
                existing.as_deref(),
                &deployment.content,
            )?);
        }
        Ok(deployment.content.clone())
    }

    /// Builds a write payload for at-fs; callers remain responsible for reviewed ChangeSets.
    ///
    /// # Errors
    /// Returns a typed error from rendering unsupported rules or delegated MCP.
    fn write_io(&self, deployment: &AgentDeployment) -> Result<AgentIoDocument, AgentError> {
        if deployment.kind == ItemKind::McpServer {
            return Err(AgentError::Delegate {
                source: AgentDelegateFailure::new("MCP writing is delegated to TM adapters"),
            });
        }
        Ok(AgentIoDocument {
            path: deployment.path.clone(),
            content: self.render_io(deployment)?,
        })
    }

    /// Validates that a selected path exists, has the expected kind, and is readable.
    ///
    /// # Errors
    /// Returns typed missing, permission, or unsupported-path errors.
    fn validate(&self, kind: ItemKind, path: &Path) -> Result<(), AgentError> {
        if path.as_os_str().is_empty() {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: kind.to_string(),
            });
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                AgentError::NotInstalled {
                    agent: self.id(),
                    path: path.to_path_buf(),
                }
            } else if source.kind() == std::io::ErrorKind::PermissionDenied {
                AgentError::Permission {
                    path: path.to_path_buf(),
                }
            } else {
                AgentError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AgentError::Unsupported {
                agent: self.id(),
                kind: format!("symlink path {}", path.display()),
            });
        }
        match kind {
            ItemKind::Skill if metadata.is_dir() => {
                std::fs::read_dir(path).map(|_| ()).map_err(|source| {
                    if source.kind() == std::io::ErrorKind::PermissionDenied {
                        AgentError::Permission {
                            path: path.to_path_buf(),
                        }
                    } else {
                        AgentError::Io {
                            path: path.to_path_buf(),
                            source,
                        }
                    }
                })
            }
            ItemKind::Rule if metadata.is_file() => {
                std::fs::File::open(path).map(|_| ()).map_err(|source| {
                    if source.kind() == std::io::ErrorKind::PermissionDenied {
                        AgentError::Permission {
                            path: path.to_path_buf(),
                        }
                    } else {
                        AgentError::Io {
                            path: path.to_path_buf(),
                            source,
                        }
                    }
                })
            }
            _ => Err(AgentError::Unsupported {
                agent: self.id(),
                kind: format!("{} at {}", kind, path.display()),
            }),
        }
    }
}

const MAX_NATIVE_ENTRIES: usize = 10_000;

fn list_native_items<F>(
    agent: &AgentId,
    scope_filter: Option<Scope>,
    paths: F,
) -> Result<Vec<NativeItem>, AgentError>
where
    F: Fn(ItemKind, Scope) -> Vec<AgentPathCandidate>,
{
    let scopes = scope_filter.map_or_else(
        || vec![Scope::Global, Scope::User, Scope::Project, Scope::Local],
        |scope| vec![scope],
    );
    let mut items = Vec::new();
    let mut entries_seen = 0;
    for scope in scopes {
        for candidate in paths(ItemKind::Skill, scope) {
            scan_skill_root(&candidate.path, scope, agent, &mut entries_seen, &mut items)?;
        }
        for candidate in paths(ItemKind::Rule, scope) {
            scan_rule_candidate(&candidate, scope, agent, &mut entries_seen, &mut items)?;
        }
    }
    items.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.scope.cmp(&right.scope))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    items.dedup_by(|left, right| {
        left.path == right.path && left.scope == right.scope && left.kind == right.kind
    });
    Ok(items)
}

fn scan_skill_root(
    root: &Path,
    scope: Scope,
    agent: &AgentId,
    entries_seen: &mut usize,
    items: &mut Vec<NativeItem>,
) -> Result<(), AgentError> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(AgentError::Permission {
                path: root.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(AgentError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    }
    let entries = std::fs::read_dir(root).map_err(|source| {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            AgentError::Permission {
                path: root.to_path_buf(),
            }
        } else {
            AgentError::Io {
                path: root.to_path_buf(),
                source,
            }
        }
    })?;
    for entry in entries {
        *entries_seen += 1;
        if *entries_seen > MAX_NATIVE_ENTRIES {
            return Err(AgentError::Unsupported {
                agent: agent.clone(),
                kind: format!("native listing limit of {MAX_NATIVE_ENTRIES} entries exceeded"),
            });
        }
        let entry = entry.map_err(|source| AgentError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let entry_path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| AgentError::Io {
                path: entry_path.clone(),
                source,
            })?
            .is_dir()
        {
            continue;
        }
        let skill_file = entry_path.join("SKILL.md");
        match std::fs::symlink_metadata(&skill_file) {
            Ok(metadata) if metadata.file_type().is_file() => {
                items.push(NativeItem {
                    kind: ItemKind::Skill,
                    scope,
                    name: entry.file_name().to_string_lossy().into_owned(),
                    path: entry_path,
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(AgentError::Permission { path: skill_file });
            }
            Err(source) => {
                return Err(AgentError::Io {
                    path: skill_file,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn scan_rule_candidate(
    candidate: &AgentPathCandidate,
    scope: Scope,
    agent: &AgentId,
    entries_seen: &mut usize,
    items: &mut Vec<NativeItem>,
) -> Result<(), AgentError> {
    let metadata = match std::fs::symlink_metadata(&candidate.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(AgentError::Permission {
                path: candidate.path.clone(),
            });
        }
        Err(source) => {
            return Err(AgentError::Io {
                path: candidate.path.clone(),
                source,
            });
        }
    };
    if metadata.file_type().is_file() {
        items.push(NativeItem {
            kind: ItemKind::Rule,
            scope,
            name: candidate
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: candidate.path.clone(),
        });
        return Ok(());
    }
    if !metadata.is_dir() || candidate.kind != "rules_dir" {
        return Ok(());
    }
    let entries = std::fs::read_dir(&candidate.path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            AgentError::Permission {
                path: candidate.path.clone(),
            }
        } else {
            AgentError::Io {
                path: candidate.path.clone(),
                source,
            }
        }
    })?;
    for entry in entries {
        *entries_seen += 1;
        if *entries_seen > MAX_NATIVE_ENTRIES {
            return Err(AgentError::Unsupported {
                agent: agent.clone(),
                kind: format!("native listing limit of {MAX_NATIVE_ENTRIES} entries exceeded"),
            });
        }
        let entry = entry.map_err(|source| AgentError::Io {
            path: candidate.path.clone(),
            source,
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| AgentError::Io {
                path: path.clone(),
                source,
            })?
            .is_file()
        {
            continue;
        }
        if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("md" | "mdc")
        ) {
            items.push(NativeItem {
                kind: ItemKind::Rule,
                scope,
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path,
            });
        }
    }
    Ok(())
}

/// The path that best explains a [`AgentError::NotInstalled`] refusal.
///
/// Detection keys on the agent's user-scope skills root, so that is the most useful path to
/// report; when an adapter declares none, the injected home is the closest honest answer.
fn missing_installation_path<A: AgentAdapter + ?Sized>(
    adapter: &A,
    env: &AgentEnvironment,
) -> PathBuf {
    adapter
        .paths(ItemKind::Skill, Scope::User, env)
        .into_iter()
        .find(|candidate| candidate.kind == "skills_dir")
        .map(|candidate| candidate.path)
        .unwrap_or_else(|| env.home.clone())
}

/// MCP-specific adapter surface supplied by the TM slice.
pub trait McpAdapterDelegate: Send + Sync {
    /// Reads and parses the selected native MCP document.
    fn read_io(&self, scope: Scope, env: &AgentEnvironment) -> Result<AgentIoDocument, AgentError>;
    /// Renders a canonical deployment through the native MCP adapter.
    fn render_io(&self, deployment: &AgentDeployment) -> Result<Vec<u8>, AgentError>;
    /// Builds native MCP document bytes for the deployment.
    fn write_io(&self, deployment: &AgentDeployment) -> Result<AgentIoDocument, AgentError>;
}

/// In-process map from agent identifiers to stable shared adapter instances.
#[derive(Default)]
pub struct AgentAdapterRegistry {
    adapters: RwLock<BTreeMap<AgentId, Arc<dyn AgentAdapter>>>,
    mcp_delegates: RwLock<BTreeMap<AgentId, Arc<dyn McpAdapterDelegate>>>,
}

impl AgentAdapterRegistry {
    /// Creates a registry with all built-in agents and a generic custom adapter.
    #[must_use]
    pub fn register() -> Self {
        let registry = Self::default();
        let adapters: [Arc<dyn AgentAdapter>; 4] = [
            Arc::new(crate::claude_adapter::ClaudeAdapter),
            Arc::new(crate::codex_adapter::CodexAdapter),
            Arc::new(crate::omp_adapter::OmpAdapter),
            Arc::new(crate::cursor_adapter::CursorAdapter),
        ];
        for adapter in adapters {
            if let Ok(mut registered) = registry.adapters.write() {
                registered.insert(adapter.id(), adapter);
            }
        }
        let adapters: [Arc<dyn AgentAdapter>; 2] = [
            Arc::new(crate::omp_adapter::OmpAdapter),
            Arc::new(crate::cursor_adapter::CursorAdapter),
        ];
        for adapter in adapters {
            if let Ok(mut registered) = registry.adapters.write() {
                registered.insert(adapter.id(), adapter);
            }
        }
        let custom: Arc<dyn AgentAdapter> = Arc::new(GenericCustomAdapter);
        if let Ok(mut adapters) = registry.adapters.write() {
            adapters.insert(AgentId::Custom(String::from("generic")), custom);
        }
        registry
    }

    /// Registers or replaces one adapter instance.
    ///
    /// # Errors
    /// Returns [`AgentError::RegistryPoisoned`] if the internal registry lock is poisoned.
    pub fn insert(&self, adapter: Arc<dyn AgentAdapter>) -> Result<(), AgentError> {
        self.adapters
            .write()
            .map_err(|_| AgentError::RegistryPoisoned)?
            .insert(adapter.id(), adapter);
        Ok(())
    }

    /// Returns the same registered adapter instance on every lookup.
    ///
    /// # Errors
    /// Returns [`AgentError::RegistryPoisoned`] if the internal registry lock is poisoned.
    pub fn get(&self, id: &AgentId) -> Result<Option<Arc<dyn AgentAdapter>>, AgentError> {
        Ok(self
            .adapters
            .read()
            .map_err(|_| AgentError::RegistryPoisoned)?
            .get(id)
            .cloned())
    }

    /// Registers the MCP implementation owned by the corresponding TM story.
    ///
    /// # Errors
    /// Returns [`AgentError::RegistryPoisoned`] if the delegate map lock is poisoned.
    pub fn register_mcp_delegate(
        &self,
        id: AgentId,
        delegate: Arc<dyn McpAdapterDelegate>,
    ) -> Result<(), AgentError> {
        self.mcp_delegates
            .write()
            .map_err(|_| AgentError::RegistryPoisoned)?
            .insert(id, delegate);
        Ok(())
    }

    fn ensure_mcp_supported(&self, id: &AgentId) -> Result<(), AgentError> {
        if self.get(id)?.is_some_and(|adapter| !adapter.supports_mcp()) {
            return Err(AgentError::Unsupported {
                agent: id.clone(),
                kind: String::from("MCP configuration"),
            });
        }
        Ok(())
    }

    /// Reads an agent document, routing MCP through its registered TM adapter.
    ///
    /// # Errors
    /// Returns the adapter/delegate error, including a typed error when no MCP delegate exists.
    pub fn read_io(
        &self,
        id: &AgentId,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Result<AgentIoDocument, AgentError> {
        if kind == ItemKind::McpServer {
            self.ensure_mcp_supported(id)?;
            let delegate = self
                .mcp_delegates
                .read()
                .map_err(|_| AgentError::RegistryPoisoned)?
                .get(id)
                .cloned()
                .ok_or_else(|| AgentError::Delegate {
                    source: AgentDelegateFailure::new("no TM MCP delegate registered"),
                })?;
            return delegate.read_io(scope, env);
        }
        self.get(id)?
            .ok_or_else(|| AgentError::Unsupported {
                agent: id.clone(),
                kind: kind.to_string(),
            })?
            .read_io(kind, scope, env)
    }

    /// Renders an agent document, routing MCP through its registered TM adapter.
    ///
    /// # Errors
    /// Returns the adapter/delegate error, including a typed error when no MCP delegate exists.
    pub fn render_io(
        &self,
        id: &AgentId,
        deployment: &AgentDeployment,
    ) -> Result<Vec<u8>, AgentError> {
        if deployment.kind == ItemKind::McpServer {
            self.ensure_mcp_supported(id)?;
            let delegate = self
                .mcp_delegates
                .read()
                .map_err(|_| AgentError::RegistryPoisoned)?
                .get(id)
                .cloned()
                .ok_or_else(|| AgentError::Delegate {
                    source: AgentDelegateFailure::new("no TM MCP delegate registered"),
                })?;
            return delegate.render_io(deployment);
        }
        self.get(id)?
            .ok_or_else(|| AgentError::Unsupported {
                agent: id.clone(),
                kind: deployment.kind.to_string(),
            })?
            .render_io(deployment)
    }

    /// Builds a write payload, routing MCP through its registered TM adapter.
    ///
    /// # Errors
    /// Returns the adapter/delegate error, including a typed error when no MCP delegate exists.
    pub fn write_io(
        &self,
        id: &AgentId,
        deployment: &AgentDeployment,
    ) -> Result<AgentIoDocument, AgentError> {
        if deployment.kind == ItemKind::McpServer {
            self.ensure_mcp_supported(id)?;
            let delegate = self
                .mcp_delegates
                .read()
                .map_err(|_| AgentError::RegistryPoisoned)?
                .get(id)
                .cloned()
                .ok_or_else(|| AgentError::Delegate {
                    source: AgentDelegateFailure::new("no TM MCP delegate registered"),
                })?;
            return delegate.write_io(deployment);
        }
        self.get(id)?
            .ok_or_else(|| AgentError::Unsupported {
                agent: id.clone(),
                kind: deployment.kind.to_string(),
            })?
            .write_io(deployment)
    }

    /// Computes canonical destination folders and the agents that read each one.
    ///
    /// Candidate roots that do not exist are still included so shared effects remain visible
    /// before first deployment. Existing paths are canonicalized; missing paths use a normalized
    /// absolute spelling, while filesystem equivalence is checked for existing roots.
    ///
    /// # Errors
    /// Returns [`AgentError::RegistryPoisoned`] when the registry lock is poisoned.
    pub fn shared_roots(
        &self,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Result<BTreeMap<PathBuf, BTreeSet<AgentId>>, AgentError> {
        let adapters: Vec<_> = self
            .adapters
            .read()
            .map_err(|_| AgentError::RegistryPoisoned)?
            .values()
            .cloned()
            .collect();
        let visible_scopes = match scope {
            Scope::Global => vec![Scope::Global],
            Scope::User => vec![Scope::User],
            Scope::Project => vec![Scope::User, Scope::Project],
            Scope::Local => vec![Scope::User, Scope::Project, Scope::Local],
        };
        let mut roots: BTreeMap<PathBuf, BTreeSet<AgentId>> = BTreeMap::new();
        for adapter in adapters {
            for visible_scope in &visible_scopes {
                for candidate in adapter.paths(ItemKind::Skill, *visible_scope, env) {
                    let key = canonical_or_normalized(&candidate.path);
                    roots.entry(key).or_default().insert(adapter.id());
                }
            }
        }
        Ok(roots)
    }
    /// Returns every registered adapter in stable identifier order.
    ///
    /// # Errors
    /// Returns [`AgentError::RegistryPoisoned`] when the registry lock is poisoned.
    pub fn adapters(&self) -> Result<Vec<Arc<dyn AgentAdapter>>, AgentError> {
        Ok(self
            .adapters
            .read()
            .map_err(|_| AgentError::RegistryPoisoned)?
            .values()
            .cloned()
            .collect())
    }
}

/// One-based line and column of the first invalid UTF-8 byte.
fn utf8_line_col(bytes: &[u8], offset: usize) -> (usize, usize) {
    let prefix = &bytes[..offset.min(bytes.len())];
    let line = 1 + prefix.iter().filter(|byte| **byte == b'\n').count();
    let line_start = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    (line, offset - line_start + 1)
}

fn plan_local_skill_change(
    agent: &AgentId,
    project: &Path,
    slug: &str,
    store_skill: &Path,
    target: &Path,
    exclude_contents: Option<&[u8]>,
    created_at_unix_seconds: u64,
) -> Result<at_model::ChangeSet, AgentError> {
    use at_model::{ChangeSet, DeployMode, ExpectedState, FileOp, OperationKind};

    if slug.is_empty() || slug == "." || slug == ".." || slug.contains(['/', '\\']) {
        return Err(AgentError::Unsupported {
            agent: agent.clone(),
            kind: String::from("invalid skill slug"),
        });
    }
    let relative_target = target
        .strip_prefix(project)
        .map_err(|error| AgentError::Io {
            path: target.to_path_buf(),
            source: std::io::Error::other(error),
        })?;
    match std::fs::symlink_metadata(target) {
        Ok(_) => {
            return Err(AgentError::Unsupported {
                agent: agent.clone(),
                kind: format!(
                    "refusing to overwrite existing skill path {}",
                    target.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(AgentError::Io {
                path: target.to_path_buf(),
                source,
            });
        }
    }
    let exclude_path = PathBuf::from(".git/info/exclude");
    let old = exclude_contents.unwrap_or_default();
    let old_text = std::str::from_utf8(old).map_err(|_| AgentError::Parse {
        path: project.join(&exclude_path),
        line: 1,
        col: 1,
    })?;
    let entry = format!("/{}", relative_target.to_string_lossy().replace('\\', "/"));
    let new_text = append_exclude_entry(old_text, &entry);
    let old_hash = at_fs::sha256_hex(old);
    let new_hash = at_fs::sha256_hex(new_text.as_bytes());
    let expected = if exclude_contents.is_some() {
        ExpectedState::hash(&old_hash).map_err(|error| AgentError::Io {
            path: project.join(&exclude_path),
            source: std::io::Error::other(error),
        })?
    } else {
        ExpectedState::absent()
    };
    let operation_kind = OperationKind::parse("deploy").map_err(|error| AgentError::Io {
        path: project.to_path_buf(),
        source: std::io::Error::other(error),
    })?;
    let change_set = ChangeSet {
        id: format!("{}-local-{}", agent, &new_hash[..16]),
        preview_id: old_hash,
        operation_kind,
        scope: Some(Scope::Local),
        project_id: None,
        summary: format!("Deploy {agent} skill `{slug}` locally"),
        ops: vec![
            FileOp::Symlink {
                link_path: relative_target.to_path_buf(),
                target: store_skill.to_path_buf(),
                mode: DeployMode::Link,
            },
            FileOp::Write {
                path: exclude_path,
                bytes: new_text.into_bytes(),
                expected,
                follow_symlink: false,
                backup: true,
            },
        ],
        created_at_unix_seconds,
    };
    change_set.validate().map_err(|error| AgentError::Io {
        path: project.to_path_buf(),
        source: std::io::Error::other(error),
    })?;
    Ok(change_set)
}

fn append_exclude_entry(contents: &str, entry: &str) -> String {
    if contents
        .lines()
        .any(|line| line.trim_end_matches('\r') == entry)
    {
        return contents.to_owned();
    }
    let separator = if contents.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if contents.is_empty() {
        return format!("{entry}{separator}");
    }
    if contents.ends_with('\n') || contents.ends_with('\r') {
        format!("{contents}{entry}{separator}")
    } else {
        format!("{contents}{separator}{entry}{separator}")
    }
}

fn select_skill_root(candidates: &[AgentPathCandidate]) -> Option<PathBuf> {
    candidates
        .iter()
        .filter(|candidate| candidate.kind == "skills_dir")
        .find(|candidate| candidate.path.exists())
        .or_else(|| {
            candidates
                .iter()
                .find(|candidate| candidate.kind == "skills_dir" && candidate.primary)
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|candidate| candidate.kind == "skills_dir")
        })
        .map(|candidate| candidate.path.clone())
}

fn canonical_or_normalized(path: &Path) -> PathBuf {
    let mut canonical = None;
    let mut suffix = Vec::new();
    if path.is_absolute() {
        let mut ancestor = path;
        loop {
            match ancestor.canonicalize() {
                Ok(real_path) => {
                    canonical = Some(real_path);
                    break;
                }
                Err(_) => {
                    if let Some(name) = ancestor.file_name() {
                        suffix.push(name.to_os_string());
                    }
                    let Some(parent) = ancestor.parent() else {
                        break;
                    };
                    ancestor = parent;
                }
            }
        }
    }
    let mut absolute = canonical.unwrap_or_else(|| path.to_path_buf());
    for component in suffix.iter().rev() {
        absolute.push(component);
    }
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(normalized.to_string_lossy().to_lowercase())
    }
    #[cfg(not(target_os = "macos"))]
    {
        normalized
    }
}

pub(crate) struct BuiltinAdapter {
    pub(crate) id: AgentId,
}

impl AgentAdapter for BuiltinAdapter {
    fn id(&self) -> AgentId {
        self.id.clone()
    }
    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        let mcp = capabilities(&self.id, &env.home);
        AgentCapabilities {
            mcp,
            supports_symlink: true,
            supports_rules: true,
        }
    }
    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        let candidates = self.paths(ItemKind::Skill, Scope::User, env);
        let skills_exist = candidates.iter().any(|candidate| candidate.path.exists());
        let config_root = match &self.id {
            AgentId::ClaudeCode => env.home.join(".claude"),
            AgentId::Codex => env.home.join(".codex"),
            AgentId::Omp => env.home.join(".omp/agent"),
            AgentId::Cursor => env.home.join(".cursor"),
            AgentId::Custom(_) => env.home.join(".agentuna/custom"),
        };
        let configured = config_root.exists();
        AgentDetection {
            state: if skills_exist {
                DetectionState::Installed
            } else if configured {
                DetectionState::ConfiguredOnly
            } else {
                DetectionState::NotInstalled
            },
            candidates,
        }
    }
    fn detect_with_version(
        &self,
        env: &AgentEnvironment,
        probe_output: Option<&str>,
    ) -> AgentDetectionWithVersion {
        AgentDetectionWithVersion {
            presence: self.detect(env),
            version: probe_output.and_then(parse_probe_version),
        }
    }
    fn paths(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        builtin_paths(&self.id, kind, scope, env)
    }
}

/// Shared [`AgentAdapter::read_io`] implementation for skills and rules.
///
/// The folder-defined custom agent overrides MCP and its rule path but keeps this listing
/// for skills, so the native-item scan lives in exactly one place.
pub(crate) fn read_io_default<A: AgentAdapter + ?Sized>(
    adapter: &A,
    kind: ItemKind,
    scope: Scope,
    env: &AgentEnvironment,
) -> Result<AgentIoDocument, AgentError> {
    if kind == ItemKind::McpServer {
        return Err(AgentError::Delegate {
            source: AgentDelegateFailure::new("no TM MCP delegate registered"),
        });
    }
    if kind == ItemKind::Skill {
        let id = adapter.id();
        let items = list_native_items(&id, Some(scope), |item_kind, item_scope| {
            adapter.paths(item_kind, item_scope, env)
        })?
        .into_iter()
        .filter(|item| item.kind == ItemKind::Skill)
        .map(|item| {
            serde_json::json!({
                "name": item.name,
                "path": item.path,
                "scope": format!("{:?}", item.scope),
            })
        })
        .collect::<Vec<_>>();
        let path = adapter
            .paths(kind, scope, env)
            .into_iter()
            .find(|candidate| candidate.primary)
            .map(|candidate| candidate.path)
            .ok_or_else(|| AgentError::Unsupported {
                agent: adapter.id(),
                kind: kind.to_string(),
            })?;
        let content = serde_json::to_vec(&items).map_err(|error| AgentError::Io {
            path: path.clone(),
            source: std::io::Error::other(error),
        })?;
        return Ok(AgentIoDocument { path, content });
    }
    let candidates = adapter.paths(kind, scope, env);
    let mut rule_items = Vec::new();
    let mut entries_seen = 0;
    let agent = adapter.id();
    for candidate in &candidates {
        scan_rule_candidate(candidate, scope, &agent, &mut entries_seen, &mut rule_items)?;
    }
    let path = rule_items
        .first()
        .map(|item| item.path.clone())
        .or_else(|| {
            candidates
                .into_iter()
                .find(|candidate| candidate.primary)
                .map(|candidate| candidate.path)
        })
        .ok_or_else(|| AgentError::Unsupported {
            agent: agent.clone(),
            kind: kind.to_string(),
        })?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(AgentError::Unsupported {
                agent,
                kind: format!("non-regular rule path {}", path.display()),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(AgentError::NotInstalled { agent, path });
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(AgentError::Permission { path });
        }
        Err(source) => return Err(AgentError::Io { path, source }),
    }
    let content = std::fs::read(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
            AgentError::Permission { path: path.clone() }
        } else {
            AgentError::Io {
                path: path.clone(),
                source,
            }
        }
    })?;
    Ok(AgentIoDocument { path, content })
}

struct GenericCustomAdapter;

impl AgentAdapter for GenericCustomAdapter {
    fn id(&self) -> AgentId {
        AgentId::Custom(String::from("generic"))
    }
    fn capabilities(&self, env: &AgentEnvironment) -> AgentCapabilities {
        AgentCapabilities {
            mcp: capabilities(&self.id(), &env.home),
            supports_symlink: false,
            supports_rules: true,
        }
    }
    fn detect(&self, env: &AgentEnvironment) -> AgentDetection {
        let candidates = self.paths(ItemKind::Skill, Scope::User, env);
        let state = if candidates.iter().any(|candidate| candidate.path.exists()) {
            DetectionState::Installed
        } else {
            DetectionState::NotInstalled
        };
        AgentDetection { state, candidates }
    }
    fn paths(
        &self,
        kind: ItemKind,
        scope: Scope,
        env: &AgentEnvironment,
    ) -> Vec<AgentPathCandidate> {
        if matches!(scope, Scope::Project | Scope::Local) {
            let Some(project) = env.project.as_ref() else {
                return Vec::new();
            };
            return builtin_candidate(kind, scope, project.join(".agentuna/custom"), Some(project));
        }
        builtin_candidate(
            kind,
            scope,
            env.home.join(".agentuna/custom"),
            env.project.as_deref(),
        )
    }
}

pub(crate) fn builtin_paths(
    id: &AgentId,
    kind: ItemKind,
    scope: Scope,
    env: &AgentEnvironment,
) -> Vec<AgentPathCandidate> {
    let project_scope = matches!(scope, Scope::Project | Scope::Local);
    if project_scope && env.project.is_none() {
        return Vec::new();
    }
    if kind == ItemKind::McpServer {
        let candidates = match id {
            AgentId::ClaudeCode if scope == Scope::User || scope == Scope::Global => {
                vec![env.home.join(".claude.json")]
            }
            AgentId::ClaudeCode if scope == Scope::Project => {
                let Some(project) = env.project.as_ref() else {
                    return Vec::new();
                };
                vec![env.home.join(".claude.json"), project.join(".mcp.json")]
            }
            AgentId::Codex if scope == Scope::User || scope == Scope::Global => {
                vec![env.home.join(".codex/config.toml")]
            }
            AgentId::Omp if scope == Scope::User || scope == Scope::Global => {
                vec![env.home.join(".omp/agent/mcp.json")]
            }
            AgentId::Omp if scope == Scope::Project => {
                let Some(project) = env.project.as_ref() else {
                    return Vec::new();
                };
                vec![project.join("mcp.json")]
            }
            AgentId::Cursor if scope == Scope::User || scope == Scope::Global => {
                vec![env.home.join(".cursor/mcp.json")]
            }
            AgentId::Cursor if scope == Scope::Project => {
                let Some(project) = env.project.as_ref() else {
                    return Vec::new();
                };
                vec![project.join(".cursor/mcp.json")]
            }
            _ => Vec::new(),
        };
        return candidates
            .into_iter()
            .enumerate()
            .map(|(index, path)| AgentPathCandidate {
                kind: String::from("mcp_file"),
                format: Some(if matches!(id, AgentId::Codex) {
                    String::from("toml")
                } else {
                    String::from("json")
                }),
                path,
                scope,
                primary: index == 0,
            })
            .collect();
    }
    if kind == ItemKind::Rule {
        return builtin_rule_paths(id, scope, env);
    }
    if kind == ItemKind::Skill {
        let roots = match (id, project_scope) {
            (AgentId::ClaudeCode, false) => vec![env.home.join(".claude/skills")],
            (AgentId::Codex, false) => vec![
                env.home.join(".codex/skills"),
                env.home.join(".agents/skills"),
            ],
            (AgentId::Omp, false) => vec![
                env.home.join(".omp/agent/managed-skills"),
                env.home.join(".omp/agent/skills"),
            ],
            (AgentId::Cursor, false) => vec![env.home.join(".cursor/skills")],
            (AgentId::Codex | AgentId::Omp, true) => vec![
                env.project
                    .as_deref()
                    .unwrap_or(Path::new("<project>"))
                    .join(".agents/skills"),
            ],
            (AgentId::ClaudeCode, true) => vec![
                env.project
                    .as_deref()
                    .unwrap_or(Path::new("<project>"))
                    .join(".claude/skills"),
            ],
            (AgentId::Cursor, true) => vec![
                env.project
                    .as_deref()
                    .unwrap_or(Path::new("<project>"))
                    .join(".cursor/skills"),
            ],
            (AgentId::Custom(_), false) => vec![env.home.join(".agentuna/custom/skills")],
            (AgentId::Custom(_), true) => vec![
                env.project
                    .as_deref()
                    .unwrap_or(Path::new("<project>"))
                    .join(".agentuna/custom/skills"),
            ],
        };
        return roots
            .into_iter()
            .enumerate()
            .map(|(index, path)| AgentPathCandidate {
                kind: String::from("skills_dir"),
                path,
                scope,
                format: None,
                primary: index == 0,
            })
            .collect();
    }
    let root = match (id, project_scope) {
        (AgentId::ClaudeCode, false) => env.home.join(".claude"),
        (AgentId::Codex, false) => env.home.join(".codex"),
        (AgentId::Omp, false) => env.home.join(".omp/agent"),
        (AgentId::Cursor, false) => env.home.join(".cursor"),
        (AgentId::Custom(_), false) => env.home.join(".agentuna/custom"),
        (AgentId::ClaudeCode, true) => env
            .project
            .as_deref()
            .unwrap_or(Path::new("<project>"))
            .join(".claude"),
        (AgentId::Codex | AgentId::Omp, true) => env
            .project
            .as_deref()
            .unwrap_or(Path::new("<project>"))
            .join(".agents"),
        (AgentId::Cursor, true) => env
            .project
            .as_deref()
            .unwrap_or(Path::new("<project>"))
            .join(".cursor"),
        (AgentId::Custom(_), true) => env
            .project
            .as_deref()
            .unwrap_or(Path::new("<project>"))
            .join(".agentuna/custom"),
    };
    builtin_candidate(kind, scope, root, env.project.as_deref())
}

fn builtin_rule_paths(
    id: &AgentId,
    scope: Scope,
    env: &AgentEnvironment,
) -> Vec<AgentPathCandidate> {
    let project = env.project.as_deref().unwrap_or(Path::new("<project>"));
    let paths: Vec<(PathBuf, &str)> = match id {
        AgentId::ClaudeCode => match scope {
            Scope::Global | Scope::User => vec![
                (env.home.join(".claude/CLAUDE.md"), "rules_file"),
                (env.home.join(".claude/claude.md"), "rules_file"),
            ],
            Scope::Project => vec![
                (project.join("CLAUDE.md"), "rules_file"),
                (project.join("CLAUDE.local.md"), "rules_file"),
                (project.join("claude.md"), "rules_file"),
            ],
            Scope::Local => vec![(project.join("CLAUDE.local.md"), "rules_file")],
        },
        AgentId::Codex => vec![(
            if matches!(scope, Scope::Global | Scope::User) {
                env.home.join(".codex/AGENTS.md")
            } else {
                project.join("AGENTS.md")
            },
            "rules_file",
        )],
        AgentId::Omp => vec![(
            if matches!(scope, Scope::Global | Scope::User) {
                env.home.join(".omp/agent/AGENTS.md")
            } else {
                project.join("AGENTS.md")
            },
            "rules_file",
        )],
        AgentId::Cursor => vec![(
            if matches!(scope, Scope::Global | Scope::User) {
                env.home.join(".cursor/rules")
            } else {
                project.join(".cursor/rules")
            },
            "rules_dir",
        )],
        AgentId::Custom(_) => vec![(
            if matches!(scope, Scope::Global | Scope::User) {
                env.home.join(".agentuna/custom/AGENTS.md")
            } else {
                project.join(".agentuna/custom/AGENTS.md")
            },
            "rules_file",
        )],
    };
    paths
        .into_iter()
        .enumerate()
        .map(|(index, (path, kind))| AgentPathCandidate {
            kind: String::from(kind),
            path,
            scope,
            format: Some(String::from("markdown")),
            primary: index == 0,
        })
        .collect()
}

fn builtin_candidate(
    kind: ItemKind,
    scope: Scope,
    root: PathBuf,
    _project: Option<&Path>,
) -> Vec<AgentPathCandidate> {
    let (kind, path, format) = match kind {
        ItemKind::Skill => ("skills_dir", root.join("skills"), None),
        ItemKind::McpServer => (
            "mcp_file",
            root.join("mcp.json"),
            Some(String::from("json")),
        ),
        ItemKind::Rule => (
            "rules_file",
            root.join("AGENTS.md"),
            Some(String::from("markdown")),
        ),
    };
    vec![AgentPathCandidate {
        kind: kind.into(),
        path,
        scope,
        format,
        primary: true,
    }]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use at_model::{AgentId, ItemKind, Scope};
    use tempfile::tempdir;

    use super::{AgentAdapterRegistry, AgentEnvironment, DetectionState};

    #[test]
    fn registry_returns_shared_instances_for_all_registered_agents() {
        let registry = AgentAdapterRegistry::register();
        for id in [
            AgentId::ClaudeCode,
            AgentId::Codex,
            AgentId::Omp,
            AgentId::Cursor,
            AgentId::Custom(String::from("generic")),
        ] {
            let first = registry
                .get(&id)
                .expect("registry lookup")
                .expect("registered");
            let second = registry
                .get(&id)
                .expect("registry lookup")
                .expect("registered");
            assert!(Arc::ptr_eq(&first, &second));
            assert_eq!(first.id(), id);
        }
    }

    #[test]
    fn project_shared_roots_couple_codex_and_omp_skills() {
        let home = tempdir().expect("home");
        let project = tempdir().expect("project");
        let env = AgentEnvironment {
            home: home.path().to_path_buf(),
            project: Some(project.path().to_path_buf()),
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        let roots = registry
            .shared_roots(Scope::Project, &env)
            .expect("shared roots");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let shared = canonical_project.join(".agents/skills");
        let claude = canonical_project.join(".claude/skills");
        #[cfg(target_os = "macos")]
        let (shared, claude) = (
            PathBuf::from(shared.to_string_lossy().to_lowercase()),
            PathBuf::from(claude.to_string_lossy().to_lowercase()),
        );
        let agents = roots.get(&shared).expect("shared Codex/omp root");
        assert!(agents.contains(&AgentId::Codex));
        assert!(agents.contains(&AgentId::Omp));
        assert_eq!(agents.len(), 2);
        assert!(
            roots
                .get(&claude)
                .is_some_and(|set| set == &std::collections::BTreeSet::from([AgentId::ClaudeCode]))
        );
    }

    #[test]
    fn detection_uses_only_injected_home_and_skill_candidates() {
        let home = tempdir().expect("home");
        let env = AgentEnvironment {
            home: home.path().to_path_buf(),
            project: None,
            variables: BTreeMap::new(),
        };
        let registry = AgentAdapterRegistry::register();
        let codex = registry
            .get(&AgentId::Codex)
            .expect("registry")
            .expect("Codex");
        assert_eq!(codex.detect(&env).state, DetectionState::NotInstalled);
        fs::create_dir_all(home.path().join(".agents/skills")).expect("skills root");
        assert_eq!(codex.detect(&env).state, DetectionState::Installed);
        assert_eq!(codex.paths(ItemKind::Skill, Scope::User, &env).len(), 2);
    }

    #[test]
    fn existing_claude_config_root_without_skills_is_configured_only() {
        let home = tempdir().expect("home");
        let env = AgentEnvironment {
            home: home.path().to_path_buf(),
            project: None,
            variables: BTreeMap::new(),
        };
        let claude = AgentAdapterRegistry::register()
            .get(&AgentId::ClaudeCode)
            .expect("registry")
            .expect("Claude adapter");
        assert_eq!(claude.detect(&env).state, DetectionState::NotInstalled);
        fs::create_dir_all(home.path().join(".claude")).expect("Claude config root");
        assert_eq!(claude.detect(&env).state, DetectionState::ConfiguredOnly);
    }

    struct MockMcpDelegate(std::sync::atomic::AtomicUsize);

    impl super::McpAdapterDelegate for MockMcpDelegate {
        fn read_io(
            &self,
            _scope: Scope,
            _env: &AgentEnvironment,
        ) -> Result<super::AgentIoDocument, super::AgentError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(super::AgentIoDocument {
                path: PathBuf::from("/fixture/mcp.json"),
                content: b"{}".to_vec(),
            })
        }

        fn render_io(
            &self,
            deployment: &super::AgentDeployment,
        ) -> Result<Vec<u8>, super::AgentError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(deployment.content.clone())
        }

        fn write_io(
            &self,
            deployment: &super::AgentDeployment,
        ) -> Result<super::AgentIoDocument, super::AgentError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(super::AgentIoDocument {
                path: deployment.path.clone(),
                content: deployment.content.clone(),
            })
        }
    }

    #[test]
    fn mcp_io_operations_are_forwarded_to_registered_tm_delegate() {
        use std::sync::atomic::Ordering;

        let registry = AgentAdapterRegistry::register();
        let delegate = Arc::new(MockMcpDelegate(std::sync::atomic::AtomicUsize::new(0)));
        registry
            .register_mcp_delegate(AgentId::Codex, delegate.clone())
            .expect("register delegate");
        let env = AgentEnvironment {
            home: PathBuf::from("/fixture/home"),
            project: None,
            variables: BTreeMap::new(),
        };
        let deployment = super::AgentDeployment {
            kind: ItemKind::McpServer,
            scope: Scope::User,
            path: PathBuf::from("/fixture/mcp.json"),
            content: b"{\"mcpServers\":{}}".to_vec(),
        };
        assert_eq!(
            registry
                .read_io(&AgentId::Codex, ItemKind::McpServer, Scope::User, &env)
                .expect("read")
                .content,
            b"{}"
        );
        assert_eq!(
            registry
                .render_io(&AgentId::Codex, &deployment)
                .expect("render"),
            deployment.content
        );
        assert_eq!(
            registry
                .write_io(&AgentId::Codex, &deployment)
                .expect("write")
                .path,
            deployment.path
        );
        assert_eq!(delegate.0.load(Ordering::SeqCst), 3);
    }
}
