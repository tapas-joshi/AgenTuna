//! Typed read command for the managed skill detail screen.

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
    let source = at_db::sources::source_for(database.connection(), &id)
        .map_err(|_| CommandError::internal("could not query skill source"))?
        .map(|source| format!("{:?}", source.kind).to_lowercase());
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
    if let Some(path) = item.source_path.as_deref() {
        match at_store::skill_package::SkillPackage::read(std::path::Path::new(path)) {
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
        path: item.source_path,
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
        editable: item.managed,
        missing,
    })
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
    let source = package_path(&item)?;
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
    let current_path = current_item.source_path.as_deref().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::NotFound,
            "The skill package is unavailable.",
        )
    })?;
    let other_path = other_item.source_path.as_deref().ok_or_else(|| {
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
                path: current_path.into(),
                deployments: current_deployments,
            },
            at_engine::skill_shadows::SkillCopyPath {
                scope: other_scope,
                path: other_path.into(),
                deployments: other_deployments,
            },
        )
    } else {
        (
            at_engine::skill_shadows::SkillCopyPath {
                scope: other_scope,
                path: other_path.into(),
                deployments: other_deployments,
            },
            at_engine::skill_shadows::SkillCopyPath {
                scope: current_scope,
                path: current_path.into(),
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
        winner: SkillCopyView {
            scope: compared.a.scope.to_string(),
            markdown: compared.a.text,
            hash: compared.a.folder_hash,
            deployments: compared.a.deployments,
        },
        shadow: SkillCopyView {
            scope: compared.b.scope.to_string(),
            markdown: compared.b.text,
            hash: compared.b.folder_hash,
            deployments: compared.b.deployments,
        },
        identical: compared.identical,
        diff,
    })
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
        items
            .into_iter()
            .next()
            .filter(|item| item.kind == at_model::ItemKind::Skill)
            .and_then(|item| item.source_path)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::NotFound,
                    "The skill package path is unavailable.",
                )
            })?
    };
    tauri::async_runtime::spawn_blocking(move || reveal_skill_path(&path))
        .await
        .map_err(|_| CommandError::internal("file manager task failed"))?
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
    let source = package_path(&item)?;
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
    let source = package_path(&item)?;
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
    if preview_identity(&expected_hash, &content) != preview_id {
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
        preview_id,
        operation_kind: at_model::change_set::OperationKind::parse("edit_skill")
            .map_err(|_| CommandError::internal("skill edit operation kind is invalid"))?,
        scope: Some(item.scope),
        project_id: item.project_id.clone(),
        summary: format!("Edited skill {}", item.name),
        ops: vec![at_model::FileOp::Write {
            path: relative.clone(),
            bytes: content.into_bytes(),
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

fn package_path(item: &at_db::shared::ItemRow) -> Result<std::path::PathBuf, CommandError> {
    item.source_path
        .as_deref()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::NotFound,
                "The skill package path is unavailable.",
            )
        })
}
