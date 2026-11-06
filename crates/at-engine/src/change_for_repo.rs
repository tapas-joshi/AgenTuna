//! Change-for-this-repository intents and reviewed ChangeSet planning (TA-008).

use std::path::PathBuf;

use at_model::{
    AgentId, ChangeSet, Definition, ExpectedState, FileOp, ItemId, ItemKind, OperationKind, Scope,
    ScopeSnapshot,
};
use at_store::manifest::{DisabledAgents, StoreManifest};
use similar::TextDiff;

use crate::local_scope::{LocalLayer, resolve_with_local_and_tombstones};
use crate::locks::{LockStore, ensure_unlocked};

use crate::tombstones::ManifestTombstones;

/// User intent requested from Effective config.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeForRepoIntent {
    /// Disable an inherited item at Project or Local scope.
    DisableInRepo {
        /// Scope to hold the tombstone.
        scope: Scope,
    },
    /// Copy an inherited definition into Project scope.
    OverrideInProject,
    /// Navigate to the defining scope without writing.
    EditUserDefinition,
}

/// Store state and resolved definitions used to create a reviewed intent plan.
pub struct ChangeForRepoRequest<'a> {
    /// Stable identity of the item.
    pub item_id: ItemId,
    /// Item kind.
    pub kind: ItemKind,
    /// Name used for the project definition filename.
    pub name: String,
    /// Canonical definition bytes for a Project override.
    pub definition_bytes: Vec<u8>,
    /// Project definition for the proposed override.
    pub project_definition: Definition,
    /// Scope snapshots loaded from Global, User and Project stores.
    pub base_layers: &'a [ScopeSnapshot],
    /// Local definitions and Local availability state.
    pub local_layer: &'a LocalLayer,
    /// Manifests loaded from Global and User stores.
    pub inherited_manifests: &'a [(Scope, StoreManifest)],
    /// Current Project manifest.
    pub project_manifest: &'a StoreManifest,
    /// Current Local manifest.
    pub local_manifest: &'a StoreManifest,
    /// Project store.json path.
    pub project_store_path: PathBuf,
    /// Local store.json path.
    pub local_store_path: PathBuf,
    /// Project definitions root.
    pub project_definitions: PathBuf,
    /// Lock store used to refuse locked changes and recompute effective config.
    pub locks: Option<&'a dyn LockStore>,
    /// Optional agent filter for tombstones and resolution.
    pub agent: Option<&'a AgentId>,
    /// Project identity carried into the ChangeSet.
    pub project_id: String,
}

/// Reviewed plan output, including diffs and navigation-only results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeForRepoPlan {
    /// File operations to apply; empty for navigation-only intent.
    pub change_set: Option<ChangeSet>,
    /// Unified diff text for each file operation.
    pub diffs: Vec<String>,
    /// Navigation target for EditUserDefinition.
    pub navigate_to: Option<(Scope, ItemId)>,
    /// Resulting effective winner for the changed item.
    pub winner_scope: Option<Scope>,
    /// First shadowed definition scope, retained for simple consumers.
    pub shadowed_scope: Option<Scope>,
    /// Every shadowed definition scope, ordered nearest to farthest.
    pub shadowed_scopes: Vec<Scope>,
}

/// Planner errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChangeForRepoError {
    /// A manifest tombstone entry was invalid.
    #[error(transparent)]
    ManifestField(#[from] at_store::manifest::FieldError),
    /// The target manifest could not be serialized.
    #[error(transparent)]
    Manifest(#[from] at_store::manifest::ManifestError),
    /// Locked item cannot be changed in a deeper scope.
    #[error("item is locked: {0}")]
    Locked(String),
    /// ChangeSet validation failed.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
}

/// Applies a reviewed change-for-repo plan through the safe-write pipeline.
///
/// # Errors
/// Returns filesystem, stale-state, or operation-recorder errors from at-fs.
pub fn apply_change_for_repo(
    plan: &ChangeForRepoPlan,
    context: &mut at_fs::ApplyContext<'_>,
    roots: &at_fs::ApplyRoots,
) -> Result<Option<at_fs::ApplyOutcome>, at_fs::ChangeSetApplyError> {
    plan.change_set
        .as_ref()
        .map(|change_set| at_fs::apply_with_roots(change_set, context, roots))
        .transpose()
}

/// Builds the exact reviewed plan for one change-for-this-repo intent.
///
/// Tombstones are constructed from the current manifest in memory; no store is written while
/// planning. The resolved winner and shadow chain are then recomputed from the supplied store
/// snapshots and resulting manifest tombstones.
///
/// # Errors
/// Returns [`ChangeForRepoError::Locked`] for locked items and typed manifest or ChangeSet
/// errors for invalid inputs or output paths.
pub fn plan_change_for_repo(
    intent: ChangeForRepoIntent,
    request: &ChangeForRepoRequest<'_>,
) -> Result<ChangeForRepoPlan, ChangeForRepoError> {
    if matches!(intent, ChangeForRepoIntent::EditUserDefinition) {
        let winner = resolve_current(request);
        return Ok(ChangeForRepoPlan {
            change_set: None,
            diffs: Vec::new(),
            navigate_to: Some((winner.scope, request.item_id)),
            winner_scope: Some(winner.scope),
            shadowed_scope: None,
            shadowed_scopes: Vec::new(),
        });
    }
    if let Some(locks) = request.locks {
        ensure_unlocked(locks, request.item_id)
            .map_err(|error| ChangeForRepoError::Locked(error.to_string()))?;
    }

    let (path, before, after, write_scope, projected_layers, projected_project, projected_local) =
        match intent {
            ChangeForRepoIntent::DisableInRepo { scope } => {
                if !matches!(scope, Scope::Project | Scope::Local) {
                    return Err(at_model::ChangeSetError::InvalidPath {
                        path: scope.to_string(),
                        reason: "disable-in-repo requires Project or Local scope".to_owned(),
                    }
                    .into());
                }
                let mut project = request.project_manifest.clone();
                let mut local = request.local_manifest.clone();
                let agents = request.agent.map_or(DisabledAgents::All, |agent| {
                    DisabledAgents::Only(vec![agent.clone()])
                });
                let (path, before, after) = if scope == Scope::Project {
                    project.disable_inherited(request.item_id, request.kind, agents.clone())?;
                    (
                        request.project_store_path.clone(),
                        request.project_manifest.to_bytes()?,
                        project.to_bytes()?,
                    )
                } else {
                    local.disable_inherited(request.item_id, request.kind, agents)?;
                    (
                        request.local_store_path.clone(),
                        request.local_manifest.to_bytes()?,
                        local.to_bytes()?,
                    )
                };
                (
                    path,
                    before,
                    after,
                    scope,
                    request.base_layers.to_vec(),
                    project,
                    local,
                )
            }
            ChangeForRepoIntent::OverrideInProject => {
                let path = request
                    .project_definitions
                    .join(format!("{}.json", request.name));
                let mut layers = request.base_layers.to_vec();
                let project = layers
                    .iter_mut()
                    .find(|layer| layer.scope == Scope::Project);
                if let Some(project) = project {
                    project.definitions.retain(|definition| {
                        definition.logical_id() != request.project_definition.logical_id()
                    });
                    let mut definition = request.project_definition.clone();
                    definition.path = path.clone();
                    project.definitions.push(definition);
                } else {
                    let mut definition = request.project_definition.clone();
                    definition.path = path.clone();
                    layers.push(ScopeSnapshot {
                        scope: Scope::Project,
                        definitions: vec![definition],
                    });
                }
                (
                    path,
                    Vec::new(),
                    request.definition_bytes.clone(),
                    Scope::Project,
                    layers,
                    request.project_manifest.clone(),
                    request.local_manifest.clone(),
                )
            }
            ChangeForRepoIntent::EditUserDefinition => {
                return Err(at_model::ChangeSetError::InvalidPath {
                    path: request.project_definitions.display().to_string(),
                    reason: "navigation-only intent cannot create a ChangeSet".to_owned(),
                }
                .into());
            }
        };

    let mut manifests = request.inherited_manifests.to_vec();
    manifests.retain(|(scope, _)| !matches!(scope, Scope::Project | Scope::Local));
    manifests.push((Scope::Project, projected_project));
    manifests.push((Scope::Local, projected_local));
    let mut tombstones = ManifestTombstones::new();
    for (scope, manifest) in &manifests {
        tombstones.insert_manifest(*scope, manifest);
    }
    let projected = resolve_with_local_and_tombstones(
        &projected_layers,
        request.local_layer,
        request.locks,
        Some(&tombstones),
        request.agent,
    )
    .resolved;
    let original = resolve_current(request);
    let winner = projected
        .items
        .iter()
        .find(|item| item.logical_id == original.logical_id);
    let mut shadowed_scopes: Vec<_> = winner
        .into_iter()
        .flat_map(|winner| {
            projected
                .shadowed
                .iter()
                .filter(move |item| item.logical_id == winner.logical_id)
                .map(|item| item.scope)
        })
        .collect();
    shadowed_scopes.sort_by_key(|scope| std::cmp::Reverse(scope.precedence()));
    let shadowed_scope = shadowed_scopes.first().copied();

    let expected = match intent {
        ChangeForRepoIntent::OverrideInProject | ChangeForRepoIntent::EditUserDefinition => {
            ExpectedState::absent()
        }
        ChangeForRepoIntent::DisableInRepo { .. } => {
            ExpectedState::hash(at_fs::sha256_hex(&before))?
        }
    };
    let diff = TextDiff::from_lines(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after),
    )
    .unified_diff()
    .to_string();
    let id = uuid::Uuid::now_v7().to_string();
    let op_kind = match request.kind {
        ItemKind::Skill => "edit_skill",
        ItemKind::McpServer => "edit_mcp",
        ItemKind::Rule => "edit_rule",
    };
    let set = ChangeSet {
        id: id.clone(),
        preview_id: id,
        operation_kind: OperationKind::parse(op_kind)?,
        scope: Some(write_scope),
        project_id: Some(request.project_id.clone()),
        summary: "Change item for this repository".to_owned(),
        ops: vec![FileOp::Write {
            path,
            bytes: after,
            expected,
            follow_symlink: false,
            backup: true,
        }],
        created_at_unix_seconds: 0,
    };
    set.validate()?;
    Ok(ChangeForRepoPlan {
        change_set: Some(set),
        diffs: vec![diff],
        navigate_to: None,
        winner_scope: winner.map(|item| item.scope),
        shadowed_scope,
        shadowed_scopes,
    })
}

fn resolve_current(request: &ChangeForRepoRequest<'_>) -> crate::resolve::ResolvedItem {
    let mut tombstones = ManifestTombstones::new();
    for (scope, manifest) in request.inherited_manifests {
        tombstones.insert_manifest(*scope, manifest);
    }
    tombstones.insert_manifest(Scope::Project, request.project_manifest);
    tombstones.insert_manifest(Scope::Local, request.local_manifest);
    let resolved = resolve_with_local_and_tombstones(
        request.base_layers,
        request.local_layer,
        request.locks,
        Some(&tombstones),
        request.agent,
    )
    .resolved;
    let logical_id = request.project_definition.logical_id();
    resolved
        .items
        .into_iter()
        .find(|item| item.logical_id == logical_id)
        .unwrap_or_else(|| crate::resolve::ResolvedItem {
            logical_id,
            scope: Scope::User,
            definition: request.project_definition.clone(),
            lock: None,
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_scope::{LocalLayer, LocalScope};
    use crate::locks::{LockId, LockRecord, MemoryLockStore};
    use at_model::{Lock, Scope};
    use at_store::layout::{StoreId, StoreLayout};
    use at_store::local_store::LocalStorePaths;

    fn definition(scope_root: &std::path::Path, item_id: ItemId) -> Definition {
        Definition {
            kind: ItemKind::Skill,
            name: "sample".to_owned(),
            declared_id: None,
            item_id: Some(item_id),
            path: scope_root.join("skills/sample/SKILL.md"),
        }
    }

    #[test]
    fn inherited_disable_writes_only_project_tombstone_and_preserves_user_manifest() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path();
        let id = ItemId::new();
        let project = StoreManifest::new(Scope::Project, StoreId::new());
        let local = StoreManifest::new(Scope::Local, StoreId::new());
        let user = StoreManifest::new(Scope::User, StoreId::new());
        let user_bytes = user.to_bytes().expect("user bytes");
        let user_scope = StoreLayout::new(Scope::User, root.join("user"));
        let project_scope = StoreLayout::new(Scope::Project, root.join("project-store"));
        let layers = vec![ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![definition(user_scope.root(), id)],
        }];
        std::fs::create_dir_all(root.join(".git")).expect("git fixture");
        let local_paths = LocalStorePaths::from_project(&project_scope).expect("local paths");
        let local_scope = LocalScope::detect(root, local_paths);
        let local_layer = LocalLayer::empty(local_scope.clone());
        let inherited = vec![(Scope::User, user)];
        let agent = AgentId::Codex;
        let project_definition = definition(user_scope.root(), id);
        let request = ChangeForRepoRequest {
            item_id: id,
            kind: ItemKind::Skill,
            name: "sample".to_owned(),
            definition_bytes: b"{\"name\":\"sample\"}".to_vec(),
            project_definition,
            base_layers: &layers,
            local_layer: &local_layer,
            inherited_manifests: &inherited,
            project_manifest: &project,
            local_manifest: &local,
            project_store_path: project_scope.manifest_path(),
            local_store_path: local_scope.store().store_file(),
            project_definitions: project_scope.skills_dir(),
            locks: None,
            agent: Some(&agent),
            project_id: "fixture".to_owned(),
        };
        let plan = plan_change_for_repo(
            ChangeForRepoIntent::DisableInRepo {
                scope: Scope::Project,
            },
            &request,
        )
        .expect("plan");
        let set = plan.change_set.expect("changeset");
        assert_eq!(set.ops.len(), 1);
        assert_eq!(
            set.ops[0].destination(),
            Some(request.project_store_path.as_path())
        );
        let FileOp::Write { bytes, .. } = &set.ops[0] else {
            panic!("write op")
        };
        let updated = StoreManifest::parse(bytes).expect("updated manifest");
        assert_eq!(updated.disabled().len(), 1);
        assert_eq!(updated.disabled()[0].item_id, id);
        assert_eq!(
            updated.disabled()[0].agents,
            DisabledAgents::Only(vec![agent])
        );
        assert_eq!(
            inherited[0].1.to_bytes().expect("unchanged user"),
            user_bytes
        );
    }

    #[test]
    fn override_recomputes_project_winner_and_global_lock_refuses_change() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path();
        let id = ItemId::new();
        let project = StoreManifest::new(Scope::Project, StoreId::new());
        let local = StoreManifest::new(Scope::Local, StoreId::new());
        let project_scope = StoreLayout::new(Scope::Project, root.join("project-store"));
        let user_scope = StoreLayout::new(Scope::User, root.join("user"));
        let user_definition = definition(user_scope.root(), id);
        let layers = vec![ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![user_definition.clone()],
        }];
        std::fs::create_dir_all(root.join(".git")).expect("git fixture");
        let local_scope = LocalScope::detect(
            root,
            LocalStorePaths::from_project(&project_scope).expect("local paths"),
        );
        let local_layer = LocalLayer::empty(local_scope.clone());
        let inherited = Vec::new();
        let request = ChangeForRepoRequest {
            item_id: id,
            kind: ItemKind::Skill,
            name: "sample".to_owned(),
            definition_bytes: b"{\"name\":\"sample\"}".to_vec(),
            project_definition: user_definition,
            base_layers: &layers,
            local_layer: &local_layer,
            inherited_manifests: &inherited,
            project_manifest: &project,
            local_manifest: &local,
            project_store_path: project_scope.manifest_path(),
            local_store_path: local_scope.store().store_file(),
            project_definitions: project_scope.skills_dir(),
            locks: None,
            agent: None,
            project_id: "fixture".to_owned(),
        };
        let plan = plan_change_for_repo(ChangeForRepoIntent::OverrideInProject, &request)
            .expect("override");
        assert_eq!(plan.winner_scope, Some(Scope::Project));
        assert_eq!(plan.shadowed_scope, Some(Scope::User));

        let locks = MemoryLockStore::new();
        locks
            .put(LockRecord {
                id: LockId::new(),
                lock: Lock {
                    scope: Scope::Global,
                    item_id: id,
                    reason: "admin policy".to_owned(),
                    locked_by: "admin".to_owned(),
                    locked_at: time::OffsetDateTime::now_utc(),
                },
            })
            .expect("lock");
        let mut locked_request = request;
        locked_request.locks = Some(&locks);
        assert!(matches!(
            plan_change_for_repo(ChangeForRepoIntent::OverrideInProject, &locked_request),
            Err(ChangeForRepoError::Locked(reason)) if reason.contains("admin policy")
        ));
    }
    #[test]
    fn local_definition_remains_the_project_override_winner() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path();
        let id = ItemId::new();
        let project = StoreManifest::new(Scope::Project, StoreId::new());
        let local = StoreManifest::new(Scope::Local, StoreId::new());
        let project_scope = StoreLayout::new(Scope::Project, root.join("project-store"));
        let user_scope = StoreLayout::new(Scope::User, root.join("user"));
        let user_definition = definition(user_scope.root(), id);
        let layers = vec![ScopeSnapshot {
            scope: Scope::User,
            definitions: vec![user_definition.clone()],
        }];
        std::fs::create_dir_all(root.join(".git")).expect("git fixture");
        let local_scope = LocalScope::detect(
            root,
            LocalStorePaths::from_project(&project_scope).expect("local paths"),
        );
        let mut local_definition = definition(local_scope.store().root(), id);
        local_definition.path = local_scope.store().root().join("skills/sample/SKILL.md");
        let local_layer =
            LocalLayer::new(local_scope.clone(), vec![local_definition]).expect("local definition");
        let request = ChangeForRepoRequest {
            item_id: id,
            kind: ItemKind::Skill,
            name: "sample".to_owned(),
            definition_bytes: b"{\"name\":\"sample\"}".to_vec(),
            project_definition: user_definition,
            base_layers: &layers,
            local_layer: &local_layer,
            inherited_manifests: &[],
            project_manifest: &project,
            local_manifest: &local,
            project_store_path: project_scope.manifest_path(),
            local_store_path: local_scope.store().store_file(),
            project_definitions: project_scope.skills_dir(),
            locks: None,
            agent: None,
            project_id: "fixture".to_owned(),
        };
        let plan = plan_change_for_repo(ChangeForRepoIntent::OverrideInProject, &request)
            .expect("override");
        assert_eq!(plan.winner_scope, Some(Scope::Local));
        assert_eq!(plan.shadowed_scope, Some(Scope::Project));
    }
}
