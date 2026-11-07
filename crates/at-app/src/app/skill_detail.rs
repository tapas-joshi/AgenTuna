//! Typed read command for the managed skill detail screen.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use super::commands::{CommandError, CommandErrorCode};
use super::state::AppState;

/// One historical version of a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillHistoryView {
    /// Stable database identity used to load the stored file map.
    pub version_id: String,
    /// Hash of the captured content.
    pub content_hash: String,
    /// RFC 3339 time the version was captured.
    pub captured_at: String,
    /// Producing operation kind, if available.
    pub operation_kind: Option<String>,
    /// Operation summary, if available.
    pub operation_summary: Option<String>,
}

/// Historical SKILL.md compared with its current content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillVersionCompareView {
    /// Selected historical version.
    pub version_id: String,
    /// Hash recorded for the historical version.
    pub version_hash: String,
    /// Hash of the current package.
    pub current_hash: String,
    /// Unified diff from the selected version to the current document.
    pub diff: String,
}

/// One agent deployment status for this skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDeploymentView {
    /// Stable agent id.
    pub agent_id: String,
    /// Display label.
    pub agent_name: String,
    /// Deployment state.
    pub state: String,
    /// Deployment mode, if recorded.
    pub mode: Option<String>,
    /// Agent-side path, if recorded.
    pub target_path: Option<String>,
}

/// Skill detail read model from the index and package store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillDetailView {
    /// Opaque item id.
    pub item_id: String,
    /// Display name.
    pub name: String,
    /// URL-safe slug.
    pub slug: String,
    /// Scope name.
    pub scope: String,
    /// Absolute path to the store package.
    pub path: Option<String>,
    /// Content of SKILL.md; empty only when the indexed package has no readable document.
    pub markdown: String,
    /// Description parsed from the SKILL.md frontmatter.
    pub description: Option<String>,
    /// Canonical package hash.
    pub hash: Option<String>,
    /// Last update timestamp.
    pub updated_at: String,
    /// Source kind and location, if imported.
    pub source: Option<String>,
    /// Path mode from the indexed package.
    pub mode: Option<String>,
    /// Effective shadow winner, if this copy is shadowed.
    pub shadowed_by: Option<String>,
    /// Ordered scopes in the shadow chain.
    pub shadow_chain: Vec<String>,
    /// Per-agent deployment records.
    pub deployments: Vec<SkillDeploymentView>,
    /// Newest captured package versions first.
    pub history: Vec<SkillHistoryView>,
    /// Whether the backing package was unavailable at query time.
    pub missing: bool,
    /// Whether AgenTuna owns the file and permits a reviewed edit.
    pub editable: bool,
    /// Whether the last source check reported an unignored update.
    pub update_available: bool,
    /// Installed source commit, when available.
    pub source_commit_sha: Option<String>,
    /// Latest checked upstream commit, when available.
    pub source_remote_sha: Option<String>,
    /// Time the upstream source was last checked.
    pub source_last_checked_at: Option<String>,
    /// Per-skill auto-update mode: off, inherit or on.
    pub auto_update: String,
}

/// Loads one indexed skill detail; native-only skills are intentionally not editable here.
#[tauri::command]
pub fn skill_detail_load(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<SkillDetailView, CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identity"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let items = at_db::shared::items_by_ids(database.connection(), std::slice::from_ref(&id))
        .map_err(|_| CommandError::internal("could not query skill index"))?;
    let item = items
        .into_iter()
        .next()
        .filter(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill is no longer in the index.",
            )
        })?;
    let indexed = at_db::skills_index::list_skills(
        database.connection(),
        &at_db::skills_index::SkillFilter {
            scope: Some(item.scope),
            ..Default::default()
        },
    )
    .map_err(|_| CommandError::internal("could not query skill deployments"))?
    .into_iter()
    .find(|row| row.item_id.as_ref() == Some(&id));
    let agent_names = at_db::agents::list_agents_on_mac(database.connection())
        .map_err(|_| CommandError::internal("could not query agent names"))?
        .into_iter()
        .map(|agent| {
            let id = agent.id.to_string();
            let name = agent.display_name.unwrap_or_else(|| id.clone());
            (id, name)
        })
        .collect::<std::collections::HashMap<_, _>>();
    let deployments = at_db::deployments::list_deployments_for_item(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill deployments"))?
        .into_iter()
        .map(|cell| {
            let agent_id = cell.agent_id.to_string();
            SkillDeploymentView {
                agent_name: agent_names
                    .get(&agent_id)
                    .cloned()
                    .unwrap_or_else(|| agent_id.clone()),
                agent_id,
                state: format!("{:?}", cell.state).to_lowercase(),
                mode: cell.mode,
                target_path: cell.target_path,
            }
        })
        .collect();
    let history = at_db::versions::versions_for_item(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill history"))?
        .into_iter()
        .map(|version| SkillHistoryView {
            version_id: version.id.to_string(),
            content_hash: version.content_hash,
            captured_at: version.captured_at,
            operation_kind: version
                .operation_kind
                .map(|kind| format!("{:?}", kind).to_lowercase()),
            operation_summary: None,
        })
        .collect();
    let source_row = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?;
    let source = source_row
        .as_ref()
        .map(|source| format!("{:?}", source.kind).to_lowercase());
    let update_available = source_row
        .as_ref()
        .is_some_and(|source| source.update_available && source.remote_sha != source.ignored_sha);
    let auto_update = source_row
        .as_ref()
        .and_then(|source| source.auto_update)
        .map(|mode| match mode {
            at_db::sources::AutoUpdate::Off => "off",
            at_db::sources::AutoUpdate::On => "on",
            at_db::sources::AutoUpdate::Inherit => "inherit",
        })
        .unwrap_or("inherit")
        .to_owned();
    let shadow_chain = at_engine::skill_shadows::indexed_skill_shadows(database.connection())
        .map_err(|_| CommandError::internal("could not query skill shadow chain"))?
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&item.name))
        .map(|(_, chain)| {
            std::iter::once(chain.winner.to_string())
                .chain(chain.shadowed.into_iter().map(|scope| scope.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut current_hash = item.content_hash.clone();
    let mut markdown = String::new();
    let mut missing = false;
    let resolved_path = resolve_item_path(&item, &state);
    if let Some(path) = resolved_path.as_deref() {
        match at_store::skill_package::SkillPackage::read(path) {
            Ok(package) => {
                if let Some(file) = package.package.get("SKILL.md") {
                    current_hash = Some(package.hash.to_string());
                    markdown = String::from_utf8_lossy(&file.bytes).into_owned();
                } else {
                    missing = true;
                }
            }
            Err(_) => missing = true,
        }
    } else {
        missing = true;
    }
    let description = markdown.lines().find_map(|line| {
        line.strip_prefix("description:")
            .map(str::trim)
            .map(str::to_owned)
    });
    Ok(SkillDetailView {
        item_id: id.to_string(),
        name: item.name,
        slug: item.slug,
        scope: item.scope.to_string(),
        path: resolved_path
            .map(|p| p.display().to_string())
            .or(item.source_path),
        markdown,
        description,
        hash: current_hash,
        updated_at: item.updated_at,
        source,
        mode: item.link_target.map(|_| "link".to_owned()),
        shadowed_by: indexed
            .and_then(|row| row.shadowed_by)
            .map(|scope| scope.to_string()),
        shadow_chain,
        deployments,
        history,
        missing,
        editable: item.managed,
        update_available,
        source_commit_sha: source_row
            .as_ref()
            .and_then(|source| source.commit_sha.clone()),
        source_remote_sha: source_row
            .as_ref()
            .and_then(|source| source.remote_sha.clone()),
        source_last_checked_at: source_row
            .as_ref()
            .and_then(|source| source.last_checked_at.clone()),
        auto_update,
    })
}
/// Persists the per-skill automatic update preference.
#[tauri::command]
pub fn skill_auto_update_set(
    state: State<'_, AppState>,
    item_id: String,
    mode: String,
) -> Result<(), CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill identity"))?;
    let mode = match mode.as_str() {
        "off" => at_db::sources::AutoUpdate::Off,
        "on" => at_db::sources::AutoUpdate::On,
        "inherit" => at_db::sources::AutoUpdate::Inherit,
        _ => {
            return Err(CommandError::invalid_argument(
                "mode",
                "choose off, inherit or on",
            ));
        }
    };
    let database = state.database().map_err(CommandError::unavailable)?;
    let source = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill has no update source.",
            )
        })?;
    if !at_db::sources::set_auto_update(database.connection(), &source.id, mode)
        .map_err(|_| CommandError::internal("could not save update preference"))?
    {
        return Err(CommandError::new(
            CommandErrorCode::NotFound,
            "The skill update source was removed.",
        ));
    }
    Ok(())
}

/// Ignores the currently observed upstream commit for this skill.
#[tauri::command]
pub fn skill_update_ignore(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<(), CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid skill identity"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let source = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill has no update source.",
            )
        })?;
    let remote_sha = source.remote_sha.ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "No checked upstream version is available.",
        )
    })?;
    if !at_db::sources::set_ignored_sha(database.connection(), &source.id, Some(&remote_sha))
        .map_err(|_| CommandError::internal("could not ignore upstream version"))?
    {
        return Err(CommandError::new(
            CommandErrorCode::NotFound,
            "The skill update source was removed.",
        ));
    }
    Ok(())
}
/// Loads a captured version's document and compares it with the current package.
#[tauri::command]
pub fn skill_version_compare(
    state: State<'_, AppState>,
    item_id: String,
    version_id: String,
) -> Result<SkillVersionCompareView, CommandError> {
    let id = parse_skill_id(&item_id)?;
    let version_id = version_id
        .parse::<i64>()
        .map_err(|_| CommandError::invalid_argument("version_id", "invalid version identity"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = skill_item(database.connection(), &id)?;
    let source = package_path(&item, &state)?;
    let current = at_store::skill_package::SkillPackage::read(&source).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The current skill package is unavailable.",
        )
    })?;
    let current_file = current.package.get("SKILL.md").ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The current SKILL.md is missing.",
        )
    })?;
    let versions = at_db::versions::versions_for_item(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill history"))?;
    let position = versions
        .iter()
        .position(|version| version.id == version_id)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This historical skill version is unavailable.",
            )
        })?;
    let selected = &versions[position];
    let current_hash = current.hash.to_string();
    let previous_bytes = if selected.content_hash == current_hash {
        current_file.bytes.clone()
    } else {
        let newer = versions
            .get(position.checked_sub(1).ok_or_else(|| {
                CommandError::unavailable(
                    "The stored document for this version is no longer available.",
                )
            })?)
            .ok_or_else(|| {
                CommandError::unavailable(
                    "The stored document for this version is no longer available.",
                )
            })?;
        let newer_content = at_db::versions::version_content(database.connection(), newer.id)
            .map_err(|_| CommandError::internal("could not load the newer version file map"))?
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::NotFound,
                    "The newer history entry is unavailable.",
                )
            })?;
        let file = newer_content
            .files
            .iter()
            .find(|file| {
                file.path.as_deref().is_some_and(|path| {
                    std::path::Path::new(path)
                        .file_name()
                        .is_some_and(|name| name == "SKILL.md")
                }) && file.backup_id.is_some()
                    && file.before_hash.is_some()
            })
            .ok_or_else(|| {
                CommandError::unavailable(
                    "The backup for this historical document is no longer available.",
                )
            })?;
        let backup_id = file
            .backup_id
            .as_deref()
            .ok_or_else(|| CommandError::internal("version file backup identity is missing"))?;
        let backup = at_db::backups::backup_by_id(database.connection(), backup_id)
            .map_err(|_| CommandError::internal("could not query the historical document backup"))?
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::NotFound,
                    "The historical document backup is unavailable.",
                )
            })?;
        let backup_path = backup
            .backup_path
            .ok_or_else(|| CommandError::internal("historical document backup has no path"))?;
        let bytes = std::fs::read(&backup_path).map_err(|_| {
            CommandError::unavailable("The historical document backup could not be read.")
        })?;
        if file.before_hash.as_deref() != Some(at_fs::sha256_hex(&bytes).as_str()) {
            return Err(CommandError::unavailable(
                "The historical document backup failed its integrity check.",
            ));
        }
        bytes
    };
    Ok(SkillVersionCompareView {
        version_id: version_id.to_string(),
        version_hash: selected.content_hash.clone(),
        current_hash,
        diff: at_fs::diff::unified("SKILL.md", Some(&previous_bytes), &current_file.bytes),
    })
}

/// One side of a skill copy comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillCopyView {
    /// Scope holding the copy.
    pub scope: String,
    /// Complete SKILL.md text.
    pub markdown: String,
    /// Hash of the complete package tree.
    pub hash: String,
    /// Agent ids with deployments of this copy.
    pub deployments: Vec<String>,
    /// Absolute package folder path.
    pub path: String,
    /// RFC 3339 modification time of SKILL.md.
    pub mtime: String,
}

/// Typed pairwise comparison payload for a shadowed skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillCompareView {
    /// Higher-precedence copy.
    pub winner: SkillCopyView,
    /// Lower-precedence copy.
    pub shadow: SkillCopyView,
    /// Whether the complete package contents match.
    pub identical: bool,
    /// Unified diff between the shadow and winner document.
    pub diff: String,
}

/// Loads and compares two indexed copies of one skill.
#[tauri::command]
pub fn skill_compare_copies(
    state: State<'_, AppState>,
    item_id: String,
    other_scope: String,
) -> Result<SkillCompareView, CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identity"))?;
    let scope = other_scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("other_scope", "unknown scope"))?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let rows = at_db::skills_index::list_skills(database.connection(), &Default::default())
        .map_err(|_| CommandError::internal("could not query skill copies"))?;
    let current = rows
        .iter()
        .find(|row| row.item_id.as_ref() == Some(&id))
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill is no longer in the index.",
            )
        })?;
    let other = rows
        .iter()
        .find(|row| row.name.eq_ignore_ascii_case(&current.name) && row.scope == scope)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The other skill copy is no longer indexed.",
            )
        })?;
    let ids = [Some(id), other.item_id];
    let ids = ids.into_iter().flatten().collect::<Vec<_>>();
    let items = at_db::shared::items_by_ids(database.connection(), &ids)
        .map_err(|_| CommandError::internal("could not query skill paths"))?;
    let current_item = items.iter().find(|item| item.id == id).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "This skill is no longer in the index.",
        )
    })?;
    let other_item = other
        .item_id
        .and_then(|other_id| items.iter().find(|item| item.id == other_id))
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The other skill copy is no longer indexed.",
            )
        })?;
    let current_path = resolve_item_path(current_item, &state).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The skill package is unavailable.",
        )
    })?;
    let other_path = resolve_item_path(other_item, &state).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The other skill package is unavailable.",
        )
    })?;
    let current_deployments =
        at_db::deployments::list_deployments_for_item(database.connection(), &id)
            .map_err(|_| CommandError::internal("could not query skill deployments"))?
            .into_iter()
            .map(|cell| cell.agent_id.to_string())
            .collect();
    let other_id = other_item.id;
    let other_deployments =
        at_db::deployments::list_deployments_for_item(database.connection(), &other_id)
            .map_err(|_| CommandError::internal("could not query skill deployments"))?
            .into_iter()
            .map(|cell| cell.agent_id.to_string())
            .collect();
    let current_scope = current.scope;
    let other_scope = other.scope;
    let (winner, shadow) = if current_scope.precedence() >= other_scope.precedence() {
        (
            at_engine::skill_shadows::SkillCopyPath {
                scope: current_scope,
                path: current_path,
                deployments: current_deployments,
            },
            at_engine::skill_shadows::SkillCopyPath {
                scope: other_scope,
                path: other_path,
                deployments: other_deployments,
            },
        )
    } else {
        (
            at_engine::skill_shadows::SkillCopyPath {
                scope: other_scope,
                path: other_path,
                deployments: other_deployments,
            },
            at_engine::skill_shadows::SkillCopyPath {
                scope: current_scope,
                path: current_path,
                deployments: current_deployments,
            },
        )
    };
    let compared =
        at_engine::skill_shadows::compare_copies_from_paths(&winner, &shadow).map_err(|_| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "A skill package could not be read.",
            )
        })?;
    let diff = at_fs::diff::unified(
        "SKILL.md",
        Some(compared.b.text.as_bytes()),
        compared.a.text.as_bytes(),
    );
    Ok(SkillCompareView {
        winner: copy_view(&compared.a),
        shadow: copy_view(&compared.b),
        identical: compared.identical,
        diff,
    })
}

/// Serializes one compared copy for the compare-copies dialog.
fn copy_view(copy: &at_engine::skill_shadows::SkillCopy) -> SkillCopyView {
    let mtime = time::OffsetDateTime::from(copy.mtime)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("unknown"));
    SkillCopyView {
        scope: copy.scope.to_string(),
        markdown: copy.text.clone(),
        hash: copy.folder_hash.clone(),
        deployments: copy.deployments.clone(),
        path: copy.path.display().to_string(),
        mtime,
    }
}

/// Request to keep one copy of a shadowed skill and overwrite the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillKeepCopyRequest {
    /// Stable skill identity in the scope the dialog was opened from.
    pub item_id: String,
    /// Scope whose copy survives.
    pub keep_scope: String,
    /// Scope whose copy is replaced by the kept version.
    pub drop_scope: String,
}

/// Read-only review of one keep-copy plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillKeepCopyPreviewView {
    /// Identity of the exact cached plan the apply command will execute.
    pub preview_id: String,
    /// Package-relative paths the reviewed operation will change.
    pub files: Vec<String>,
    /// Unified SKILL.md diff from the replaced copy to the kept copy.
    pub diff: String,
    /// Whether both packages already match, making apply a no-op.
    pub identical: bool,
}

/// Outcome of applying a keep-copy plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillKeepCopyResultView {
    /// Operation journal identity; absent when nothing changed.
    pub operation_id: Option<String>,
    /// Package hash of the replaced copy after the operation.
    pub expected_hash: String,
}

struct CachedKeepPlan {
    set: at_model::ChangeSet,
    store_root: std::path::PathBuf,
    backup_root: std::path::PathBuf,
    authorized_roots: Vec<std::path::PathBuf>,
    drop_item_id: at_model::ItemId,
    drop_scope: at_model::Scope,
    #[expect(dead_code)]
    project_id: Option<String>,
    drop_dir: std::path::PathBuf,
}

static KEEP_PLANS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, CachedKeepPlan>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Reviews the exact package overwrite that keeps one copy and replaces the other.
#[tauri::command]
pub fn skill_keep_copy_preview(
    state: State<'_, AppState>,
    request: SkillKeepCopyRequest,
) -> Result<SkillKeepCopyPreviewView, CommandError> {
    skill_keep_copy_preview_inner(&state, request)
}

/// Body of [`skill_keep_copy_preview`] over injected state, callable from tests.
///
/// # Errors
/// See [`skill_keep_copy_preview`].
pub(crate) fn skill_keep_copy_preview_inner(
    state: &AppState,
    request: SkillKeepCopyRequest,
) -> Result<SkillKeepCopyPreviewView, CommandError> {
    let id = parse_skill_id(&request.item_id)?;
    let keep_scope = request
        .keep_scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("keep_scope", "unknown scope"))?;
    let drop_scope = request
        .drop_scope
        .parse::<at_model::Scope>()
        .map_err(|_| CommandError::invalid_argument("drop_scope", "unknown scope"))?;
    if keep_scope == drop_scope {
        return Err(CommandError::invalid_argument(
            "drop_scope",
            "the kept and replaced copies must be different scopes",
        ));
    }
    let database = state.database().map_err(CommandError::unavailable)?;
    let (keep_item, drop_item) = shadow_copy_items(database.connection(), &id, drop_scope)?;
    let project_root = drop_item.project_id.as_deref().and_then(|project_id| {
        at_db::projects::find_project_by_id(database.connection(), project_id)
            .ok()
            .flatten()
            .map(|project| std::path::PathBuf::from(project.path))
    });
    let paths = super::paths::resolve_command_paths(state, project_root.as_deref())?;
    let keep_dir = package_path(&keep_item, state)?;
    let drop_dir = package_path(&drop_item, state)?;
    let keep = at_store::skill_package::SkillPackage::read(&keep_dir).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The kept skill package is unavailable.",
        )
    })?;
    let drop = at_store::skill_package::SkillPackage::read(&drop_dir).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The replaced skill package is unavailable.",
        )
    })?;
    let store_root = drop_dir
        .parent()
        .and_then(std::path::Path::parent)
        .map(|parent| parent.to_path_buf())
        .ok_or_else(|| CommandError::internal("skill store path is invalid"))?;
    let mut ops = Vec::new();
    let mut files = Vec::new();
    for relative in keep.package.paths() {
        let bytes = keep
            .package
            .get(relative)
            .map(|file| file.bytes.clone())
            .ok_or_else(|| CommandError::internal("the kept package changed during review"))?;
        let destination = drop_dir.join(relative);
        let expected = match std::fs::read(&destination) {
            Ok(existing) => at_model::ExpectedState::hash(at_fs::sha256_hex(&existing))
                .map_err(|_| CommandError::internal("replaced file hash is invalid"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                at_model::ExpectedState::absent()
            }
            Err(_) => {
                return Err(CommandError::new(
                    CommandErrorCode::Conflict,
                    "the replaced skill package could not be inspected safely",
                ));
            }
        };
        ops.push(at_model::FileOp::Write {
            path: destination.clone(),
            bytes,
            expected,
            follow_symlink: false,
            backup: true,
        });
        files.push((*relative).to_owned());
    }
    for relative in package_file_list(&drop_dir)? {
        if keep.package.get(&relative).is_some() {
            continue;
        }
        let bytes = std::fs::read(drop_dir.join(&relative)).map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "the replaced skill package changed during review",
            )
        })?;
        ops.push(at_model::FileOp::Delete {
            path: drop_dir.join(&relative),
            expected: at_model::ExpectedState::hash(at_fs::sha256_hex(&bytes))
                .map_err(|_| CommandError::internal("replaced file hash is invalid"))?,
        });
        files.push(relative);
    }
    files.sort();
    let keep_doc = keep.package.get("SKILL.md").map(|file| file.bytes.clone());
    let drop_doc = drop.package.get("SKILL.md").map(|file| file.bytes.clone());
    let diff = at_fs::diff::unified(
        "SKILL.md",
        drop_doc.as_deref(),
        keep_doc.as_deref().unwrap_or(&[]),
    );
    let identical = keep.hash == drop.hash;
    let preview_id = uuid::Uuid::now_v7().to_string();
    let change_set = at_model::ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: preview_id.clone(),
        operation_kind: at_model::change_set::OperationKind::parse("edit_skill")
            .map_err(|_| CommandError::internal("skill edit operation kind is invalid"))?,
        scope: Some(drop_scope),
        project_id: drop_item.project_id.clone(),
        summary: format!(
            "Keep {} copy of {} over {}",
            keep_scope, keep_item.name, drop_scope
        ),
        ops,
        created_at_unix_seconds: time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
    };
    let mut authorized_roots = vec![
        paths
            .home_dir()
            .ok_or_else(|| CommandError::unavailable("home directory is unavailable"))?
            .to_path_buf(),
    ];
    if let Some(root) = project_root
        .as_ref()
        .filter(|root| !authorized_roots.contains(root))
    {
        authorized_roots.push(root.clone());
    }
    for root in [keep_dir.parent(), Some(store_root.as_path())]
        .into_iter()
        .flatten()
    {
        if !authorized_roots
            .iter()
            .any(|allowed| root.starts_with(allowed))
        {
            authorized_roots.push(root.to_path_buf());
        }
    }
    KEEP_PLANS
        .lock()
        .map_err(|_| CommandError::internal("keep-copy plan cache is unavailable"))?
        .insert(
            preview_id.clone(),
            CachedKeepPlan {
                set: change_set,
                backup_root: paths.support_dir().join(at_store::layout::BACKUPS_DIR),
                store_root,
                authorized_roots,
                drop_item_id: drop_item.id,
                drop_scope,
                project_id: drop_item.project_id.clone(),
                drop_dir,
            },
        );
    Ok(SkillKeepCopyPreviewView {
        preview_id,
        files,
        diff,
        identical,
    })
}

/// Applies a reviewed keep-copy plan through the journaled safe-write pipeline.
#[tauri::command]
pub fn skill_keep_copy_apply(
    state: State<'_, AppState>,
    preview_id: String,
) -> Result<SkillKeepCopyResultView, CommandError> {
    skill_keep_copy_apply_inner(&state, &preview_id)
}

/// Body of [`skill_keep_copy_apply`] over injected state, callable from tests.
///
/// # Errors
/// See [`skill_keep_copy_apply`].
pub(crate) fn skill_keep_copy_apply_inner(
    state: &AppState,
    preview_id: &str,
) -> Result<SkillKeepCopyResultView, CommandError> {
    let cached = KEEP_PLANS
        .lock()
        .map_err(|_| CommandError::internal("keep-copy plan cache is unavailable"))?
        .remove(preview_id)
        .ok_or_else(|| {
            CommandError::invalid_argument(
                "preview_id",
                "the keep-copy review expired; compare the copies again",
            )
        })?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let roots = at_fs::changeset::ApplyRoots::new(cached.authorized_roots)
        .map_err(|_| CommandError::unavailable("authorized skill roots are unavailable"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root: &cached.store_root,
        backup_folder: &cached.backup_root,
        recorder: &mut recorder,
    };
    let outcome =
        at_fs::changeset::apply_with_roots(&cached.set, &mut context, &roots).map_err(|error| {
            match error {
                at_fs::changeset::ChangeSetApplyError::Stale(path) => CommandError::new(
                    CommandErrorCode::Conflict,
                    format!(
                        "The replaced copy changed at {}. Compare the copies again.",
                        path.display()
                    ),
                ),
                other => CommandError::from(other),
            }
        })?;
    state.record_writes(&cached.set, &cached.store_root);
    let updated = at_store::skill_package::SkillPackage::read(&cached.drop_dir)
        .map_err(|_| CommandError::internal("the replaced skill package could not be re-read"))?;
    let operation_id = outcome.operation_id.clone();
    at_db::versions::record_skill_version(
        database.connection(),
        &at_db::versions::NewVersion::new(
            &cached.drop_item_id,
            cached.drop_scope,
            &updated.hash.to_string(),
        )
        .with_path("SKILL.md")
        .with_operation(&operation_id),
    )
    .map_err(|_| {
        CommandError::internal("the copies were reconciled but history could not be recorded")
    })?;
    // The replaced store copy now holds the other copy's bytes; a linked deployment shows that
    // content, so the drop item's baseline must move with it.
    at_engine::drift::sync_item_baselines(
        database.connection(),
        &cached.drop_item_id,
        cached.drop_scope,
        Some(&cached.drop_dir),
    )
    .map_err(|error| {
        CommandError::internal(format!(
            "skill deployment baselines could not be refreshed: {error}"
        ))
    })?;
    Ok(SkillKeepCopyResultView {
        operation_id: Some(operation_id),
        expected_hash: updated.hash.to_string(),
    })
}

/// Resolves the kept and replaced item rows for one shadowed skill.
fn shadow_copy_items(
    connection: &rusqlite::Connection,
    id: &at_model::ItemId,
    drop_scope: at_model::Scope,
) -> Result<(at_db::shared::ItemRow, at_db::shared::ItemRow), CommandError> {
    let current = skill_item(connection, id)?;
    let rows = at_db::skills_index::list_skills(connection, &Default::default())
        .map_err(|_| CommandError::internal("could not query skill copies"))?;
    let other = rows
        .iter()
        .find(|row| row.name.eq_ignore_ascii_case(&current.name) && row.scope == drop_scope)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The other skill copy is no longer indexed.",
            )
        })?
        .item_id;
    let drop_item = other
        .and_then(|other_id| {
            at_db::shared::items_by_ids(connection, std::slice::from_ref(&other_id))
                .ok()
                .and_then(|items| items.into_iter().next())
        })
        .filter(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The other skill copy is no longer indexed.",
            )
        })?;
    Ok((current, drop_item))
}

/// Lists package-relative regular files beneath one skill folder, bounded.
fn package_file_list(dir: &std::path::Path) -> Result<Vec<String>, CommandError> {
    const MAX_FILES: usize = 500;
    const MAX_DEPTH: usize = 16;
    let mut out = Vec::new();
    let mut stack = vec![(std::path::PathBuf::from(dir), 0_usize)];
    while let Some((current, depth)) = stack.pop() {
        let entries = std::fs::read_dir(&current).map_err(|_| {
            CommandError::new(
                CommandErrorCode::Conflict,
                "the replaced skill package could not be listed",
            )
        })?;
        for entry in entries.flatten() {
            if out.len() >= MAX_FILES {
                return Ok(out);
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if kind.is_symlink() {
                continue;
            }
            if let Ok(relative) = path.strip_prefix(dir) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(out)
}
/// Reveals the managed skill package in the system file manager.
#[tauri::command]
pub async fn skill_reveal(state: State<'_, AppState>, item_id: String) -> Result<(), CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identity"))?;
    let path = {
        let database = state.database().map_err(CommandError::unavailable)?;
        let items = at_db::shared::items_by_ids(database.connection(), std::slice::from_ref(&id))
            .map_err(|_| CommandError::internal("could not query skill path"))?;
        let item = items
            .into_iter()
            .next()
            .filter(|item| item.kind == at_model::ItemKind::Skill)
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::NotFound,
                    "The skill package path is unavailable.",
                )
            })?;
        package_path(&item, &state)?
    };
    tauri::async_runtime::spawn_blocking(move || reveal_skill_path(&path))
        .await
        .map_err(|_| CommandError::internal("file manager task failed"))?
}
/// Reveals a managed item in the system file manager after validating its registered path.
#[tauri::command]
pub async fn reveal_item(state: State<'_, AppState>, item_id: String) -> Result<(), CommandError> {
    let id = item_id
        .parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identity"))?;
    let path = {
        let database = state.database().map_err(CommandError::unavailable)?;
        let item = at_db::shared::items_by_ids(database.connection(), std::slice::from_ref(&id))
            .map_err(|_| CommandError::internal("could not query item path"))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                CommandError::new(CommandErrorCode::NotFound, "The item is no longer indexed.")
            })?;
        let source = item.source_path.ok_or_else(|| {
            CommandError::new(CommandErrorCode::NotFound, "The item path is unavailable.")
        })?;
        let project_root = item
            .project_id
            .as_deref()
            .map(|project_id| {
                at_db::projects::find_project_by_id(database.connection(), project_id)
                    .map_err(|_| CommandError::internal("could not query the owning project"))?
                    .map(|project| std::path::PathBuf::from(project.path))
                    .ok_or_else(|| {
                        CommandError::new(
                            CommandErrorCode::NotFound,
                            "The owning project is no longer registered.",
                        )
                    })
            })
            .transpose()?;
        let paths = super::paths::resolve_process_paths(project_root.as_deref()).map_err(|_| {
            CommandError::unavailable("authorized application roots are unavailable")
        })?;
        let mut roots = vec![
            paths.support_dir().to_path_buf(),
            paths.user_store().to_path_buf(),
            paths.global_store().to_path_buf(),
        ];
        if let Some(home) = paths.home_dir() {
            roots.push(home.to_path_buf());
        }
        if let Some(project) = project_root {
            roots.push(project);
        }
        if let Some(project_store) = paths.project_store() {
            roots.push(project_store.to_path_buf());
        }
        authorized_existing_path(&std::path::PathBuf::from(source), &roots)?
    };
    tauri::async_runtime::spawn_blocking(move || reveal_skill_path(&path))
        .await
        .map_err(|_| CommandError::internal("file manager task failed"))?
}

fn authorized_existing_path(
    path: &std::path::Path,
    roots: &[std::path::PathBuf],
) -> Result<std::path::PathBuf, CommandError> {
    if !path.is_absolute() {
        return Err(CommandError::invalid_argument(
            "item_id",
            "registered item path is not absolute",
        ));
    }
    let canonical_path = path.canonicalize().map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The item path no longer exists.",
        )
    })?;
    let permitted = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| canonical_path.starts_with(root));
    if !permitted {
        return Err(CommandError::invalid_argument(
            "item_id",
            "registered item path is outside authorized roots",
        ));
    }
    Ok(canonical_path)
}

fn reveal_skill_path(path: &std::path::Path) -> Result<(), CommandError> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .status();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let result = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .status();
    let status =
        result.map_err(|_| CommandError::unavailable("could not open the file manager"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| CommandError::unavailable("file manager could not reveal the skill"))
}
/// A non-mutating edit preview for one managed SKILL.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillEditPreviewView {
    /// Current package hash bound to this preview.
    pub expected_hash: String,
    /// Stable identity for this exact content and package version.
    pub preview_id: String,
    /// Unified document diff.
    pub diff: String,
}
/// Preview an edit without writing files or database rows.
#[tauri::command]
pub fn skill_edit_preview(
    state: State<'_, AppState>,
    item_id: String,
    content: String,
) -> Result<SkillEditPreviewView, CommandError> {
    if content.len() > at_store::skill_package::SKILL_PACKAGE_MAX_FILE_BYTES as usize {
        return Err(CommandError::invalid_argument(
            "content",
            "SKILL.md exceeds the 10 MiB limit",
        ));
    }
    let id = parse_skill_id(&item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = skill_item(database.connection(), &id)?;
    if !item.managed {
        return Err(CommandError::unavailable(
            "Only AgenTuna-managed skills can be edited.",
        ));
    }
    let source = package_path(&item, &state)?;
    let loaded = at_store::skill_package::SkillPackage::read(&source).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The skill package is unavailable.",
        )
    })?;
    let old = loaded
        .package
        .get("SKILL.md")
        .ok_or_else(|| CommandError::new(CommandErrorCode::NotFound, "SKILL.md is missing."))?;
    let expected_hash = loaded.hash.to_string();
    let preview_id = preview_identity(&expected_hash, &content);
    Ok(SkillEditPreviewView {
        expected_hash,
        preview_id,
        diff: at_fs::diff::unified("SKILL.md", Some(&old.bytes), content.as_bytes()),
    })
}

/// Apply an explicitly reviewed skill edit through the journaled safe-write pipeline.
#[tauri::command]
pub fn skill_edit_apply(
    state: State<'_, AppState>,
    item_id: String,
    content: String,
    expected_hash: String,
    preview_id: String,
) -> Result<SkillEditResultView, CommandError> {
    skill_edit_apply_inner(&state, &item_id, &content, &expected_hash, &preview_id)
}

/// Body of [`skill_edit_apply`] over injected state, callable from tests.
///
/// # Errors
/// See [`skill_edit_apply`].
pub(crate) fn skill_edit_apply_inner(
    state: &AppState,
    item_id: &str,
    content: &str,
    expected_hash: &str,
    preview_id: &str,
) -> Result<SkillEditResultView, CommandError> {
    if content.len() > at_store::skill_package::SKILL_PACKAGE_MAX_FILE_BYTES as usize {
        return Err(CommandError::invalid_argument(
            "content",
            "SKILL.md exceeds the 10 MiB limit",
        ));
    }
    let id = parse_skill_id(item_id)?;
    let database = state.database().map_err(CommandError::unavailable)?;
    let item = skill_item(database.connection(), &id)?;
    if !item.managed {
        return Err(CommandError::unavailable(
            "Only AgenTuna-managed skills can be edited.",
        ));
    }
    let source = package_path(&item, state)?;
    let loaded = at_store::skill_package::SkillPackage::read(&source).map_err(|_| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The skill package is unavailable.",
        )
    })?;
    if loaded.hash.to_string() != expected_hash {
        return Err(CommandError::unavailable(
            "The skill changed after the preview. Reload and review it again.",
        ));
    }
    if preview_identity(expected_hash, content) != preview_id {
        return Err(CommandError::invalid_argument(
            "preview_id",
            "the document differs from the reviewed preview",
        ));
    }
    let old = loaded
        .package
        .get("SKILL.md")
        .ok_or_else(|| CommandError::new(CommandErrorCode::NotFound, "SKILL.md is missing."))?;
    let relative = source
        .strip_prefix(
            source
                .parent()
                .and_then(std::path::Path::parent)
                .ok_or_else(|| CommandError::internal("skill store path is invalid"))?,
        )
        .map_err(|_| CommandError::internal("skill path is outside its store"))?
        .join("SKILL.md");
    let change_set = at_model::ChangeSet {
        id: uuid::Uuid::now_v7().to_string(),
        preview_id: preview_id.to_owned(),
        operation_kind: at_model::change_set::OperationKind::parse("edit_skill")
            .map_err(|_| CommandError::internal("skill edit operation kind is invalid"))?,
        scope: Some(item.scope),
        project_id: item.project_id.clone(),
        summary: format!("Edited skill {}", item.name),
        ops: vec![at_model::FileOp::Write {
            path: relative.clone(),
            bytes: content.as_bytes().to_vec(),
            expected: at_model::ExpectedState::hash(at_fs::sha256_hex(&old.bytes))
                .map_err(|_| CommandError::internal("skill document hash is invalid"))?,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
    };
    let store_root = source
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| CommandError::internal("skill store path is invalid"))?;
    let mut recorder = at_fs::changeset::DbOperationRecorder::new(database.connection(), 30);
    let mut context = at_fs::changeset::ApplyContext {
        store_root,
        backup_folder: &store_root.join(at_store::layout::BACKUPS_DIR),
        recorder: &mut recorder,
    };
    let outcome = at_fs::changeset::apply(&change_set, &mut context).map_err(CommandError::from)?;
    state.record_writes(&change_set, store_root);
    let updated = at_store::skill_package::SkillPackage::read(&source)
        .map_err(|_| CommandError::internal("the saved skill package could not be re-read"))?;
    let relative_text = relative.to_string_lossy().into_owned();
    let operation_id = outcome.operation_id;
    at_db::versions::record_skill_version(
        database.connection(),
        &at_db::versions::NewVersion::new(&id, item.scope, &updated.hash.to_string())
            .with_path(&relative_text)
            .with_operation(&operation_id),
    )
    .map_err(|_| {
        CommandError::internal("the skill was saved but its history could not be recorded")
    })?;
    // A linked deployment reads the store through its link, so the edit changed the native
    // folder without any command writing it. Refresh the item's rows or the next scan reports
    // the edit as an external change.
    let skill_dir = source
        .parent()
        .ok_or_else(|| CommandError::internal("skill store path is invalid"))?;
    at_engine::drift::sync_item_baselines(database.connection(), &id, item.scope, Some(skill_dir))
        .map_err(|error| {
            CommandError::internal(format!(
                "skill deployment baselines could not be refreshed: {error}"
            ))
        })?;
    Ok(SkillEditResultView {
        expected_hash: updated.hash.to_string(),
        diff: at_fs::diff::unified(
            "SKILL.md",
            Some(&old.bytes),
            updated
                .package
                .get("SKILL.md")
                .map_or(&[][..], |file| file.bytes.as_slice()),
        ),
        operation_id,
    })
}

/// Result of applying a reviewed skill edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SkillEditResultView {
    /// New package hash.
    pub expected_hash: String,
    /// Applied document diff.
    pub diff: String,
    /// History operation identifier.
    pub operation_id: String,
}

fn preview_identity(expected_hash: &str, content: &str) -> String {
    at_fs::sha256_hex(format!("{expected_hash}\0{content}").as_bytes())
}

fn parse_skill_id(text: &str) -> Result<at_model::ItemId, CommandError> {
    text.parse::<at_model::ItemId>()
        .map_err(|_| CommandError::invalid_argument("item_id", "invalid item identity"))
}

fn skill_item(
    connection: &rusqlite::Connection,
    id: &at_model::ItemId,
) -> Result<at_db::shared::ItemRow, CommandError> {
    at_db::shared::items_by_ids(connection, std::slice::from_ref(id))
        .map_err(|_| CommandError::internal("could not query skill index"))?
        .into_iter()
        .next()
        .filter(|item| item.kind == at_model::ItemKind::Skill)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "This skill is no longer in the index.",
            )
        })
}

fn resolve_item_path(
    item: &at_db::shared::ItemRow,
    state: &AppState,
) -> Option<std::path::PathBuf> {
    let raw = item.source_path.as_deref()?;
    let path = std::path::Path::new(raw);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        let paths = super::paths::resolve_command_paths(state, None).ok()?;
        at_store::layout::StoreLayout::for_scope(&paths, item.scope)
            .ok()
            .map(|layout| layout.root().join(path))
    }
}

fn package_path(
    item: &at_db::shared::ItemRow,
    state: &AppState,
) -> Result<std::path::PathBuf, CommandError> {
    resolve_item_path(item, state).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The skill package path is unavailable.",
        )
    })
}

#[cfg(test)]
mod reveal_item_tests {
    use super::authorized_existing_path;

    #[test]
    fn authorized_item_reveal_requires_component_containment() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("authorized");
        std::fs::create_dir(&root).expect("authorized root");
        let item = root.join("item.json");
        std::fs::write(&item, "content").expect("item");
        assert_eq!(
            authorized_existing_path(&item, std::slice::from_ref(&root)).expect("authorized"),
            item.canonicalize().expect("canonical item")
        );

        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, "content").expect("outside item");
        assert!(authorized_existing_path(&outside, std::slice::from_ref(&root)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn authorized_item_reveal_rejects_symlink_escape() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("authorized");
        std::fs::create_dir(&root).expect("authorized root");
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, "content").expect("outside item");
        let link = root.join("linked.json");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        assert!(authorized_existing_path(&link, &[root]).is_err());
    }
}

#[cfg(test)]
mod keep_copy_drift_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use at_db::deployments::DeploymentSpec;
    use at_model::{AgentId, ItemId, ItemState, Scope};
    use at_store::layout::{Slug, StoreLayout};
    use at_store::paths::{AppPaths, PathEnvironment};

    use super::*;
    use crate::app::bridge::{EventSink, JobBridge, JobEvent, TracingOperationsLog};
    use crate::app::drift_testkit::{assert_external_edit_detected_at, assert_no_drift};

    const KEPT: &str = "---\nname: demo-skill\ndescription: kept\n---\n\nKept body.\n";
    const REPLACED: &str = "---\nname: demo-skill\ndescription: old\n---\n\nOld body.\n";

    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&self, _event: JobEvent) {}
    }

    /// One User copy to keep, one Project copy to replace, and a live link for the replaced copy.
    struct KeepCopySandbox {
        _home: at_fs::testkit::TestHome,
        state: AppState,
        keep_item: ItemId,
        target: PathBuf,
    }

    fn write_package(dir: &std::path::Path, content: &str) {
        std::fs::create_dir_all(dir).expect("package directory");
        std::fs::write(dir.join("SKILL.md"), content).expect("package document");
    }

    impl KeepCopySandbox {
        fn new() -> Self {
            let home = at_fs::testkit::TestHome::new().expect("test home");
            let paths =
                AppPaths::resolve(&PathEnvironment::default().with_home(home.home().to_path_buf()))
                    .expect("app paths");
            std::fs::create_dir_all(paths.support_dir()).expect("support directory");
            std::fs::create_dir_all(paths.backup_dir()).expect("backup directory");
            let database = at_db::Database::open_in_memory().expect("database");

            let slug = Slug::parse("demo-skill").expect("slug");
            let user_store = StoreLayout::for_scope(&paths, Scope::User).expect("user store");
            let keep_dir = user_store.skill_dir(&slug);
            write_package(&keep_dir, KEPT);
            let drop_dir = home.home().join("project-store/skills/demo-skill");
            write_package(&drop_dir, REPLACED);

            let keep_item = ItemId::new();
            let drop_item = ItemId::new();
            for (id, scope, path) in [
                (&keep_item, "user", &keep_dir),
                (&drop_item, "project", &drop_dir),
            ] {
                database
                    .connection()
                    .execute(
                        "INSERT INTO items (id, kind, scope, slug, name, source_path, managed) \
                         VALUES (?1, 'skill', ?2, 'demo-skill', 'demo-skill', ?3, 1)",
                        rusqlite::params![id.to_string(), scope, path.to_string_lossy()],
                    )
                    .expect("skill item");
            }

            let target = home.home().join("project/.claude/skills/demo-skill");
            std::fs::create_dir_all(target.parent().expect("target parent"))
                .expect("target parent");
            std::os::unix::fs::symlink(&drop_dir, &target).expect("deployment link");
            let hash = at_store::manifest::hash_skill_folder(&drop_dir)
                .expect("package hash")
                .to_string();
            let target_text = target.to_string_lossy().into_owned();
            let drop_text = drop_dir.to_string_lossy().into_owned();
            let spec = DeploymentSpec::applied(
                &drop_item,
                &AgentId::ClaudeCode,
                Scope::Project,
                ItemState::Linked,
                Some("symlink"),
                Some(&target_text),
                at_db::deployments::DeploymentHashes::in_sync(&hash),
            )
            .with_link_target(&drop_text);
            at_db::deployments::upsert_deployment(database.connection(), &spec)
                .expect("deployment row");

            let state = AppState::new(
                Arc::new(JobBridge::new(
                    Arc::new(NullSink),
                    Arc::new(TracingOperationsLog),
                )),
                None,
            )
            .with_database(database)
            .with_paths(paths);

            Self {
                _home: home,
                state,
                keep_item,
                target,
            }
        }

        fn apply(&self) -> SkillKeepCopyResultView {
            let preview = skill_keep_copy_preview_inner(
                &self.state,
                SkillKeepCopyRequest {
                    item_id: self.keep_item.to_string(),
                    keep_scope: Scope::User.to_string(),
                    drop_scope: Scope::Project.to_string(),
                },
            )
            .expect("keep-copy preview");
            assert!(!preview.identical);
            skill_keep_copy_apply_inner(&self.state, &preview.preview_id).expect("keep-copy apply")
        }
    }

    #[test]
    fn keeping_one_copy_refreshes_the_replaced_deployment_baseline() {
        let sandbox = KeepCopySandbox::new();
        assert_no_drift(&sandbox.state, "before skill_keep_copy_apply");
        let applied = sandbox.apply();
        assert!(applied.operation_id.is_some());
        assert_no_drift(&sandbox.state, "after skill_keep_copy_apply");
        assert_external_edit_detected_at(
            &sandbox.state,
            &sandbox.target.join("SKILL.md"),
            &sandbox.target,
            b"---\nname: demo-skill\n---\n\nExternal body.\n",
        );
    }
}
