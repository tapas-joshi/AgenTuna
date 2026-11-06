//! Preset CRUD, apply/unapply planning, and operation-history recording (TS-022).
//!
//! This module owns membership policy and history. The existing skill-agent and MCP planners
//! produce per-member transitions; this module merges them into one ChangeSet and applies it
//! through the authorized-root `at-fs` pipeline.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;

use at_db::{Database, DbError, presets as repository};
use at_model::{AgentId, ChangeSet, ItemId, ItemKind, OperationKind, Scope};
use thiserror::Error;

/// Destination of a preset application.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Target {
    /// Apply to one agent's User configuration.
    Agent(AgentId),
    /// Apply into one repository's Project scope.
    Repo(String),
}

impl Target {
    fn kind(&self) -> repository::PresetTargetKind {
        match self {
            Self::Agent(_) => repository::PresetTargetKind::Agent,
            Self::Repo(_) => repository::PresetTargetKind::Project,
        }
    }

    fn id(&self) -> Cow<'_, str> {
        match self {
            Self::Agent(agent) => Cow::Owned(agent.to_string()),
            Self::Repo(id) => Cow::Borrowed(id),
        }
    }
}

/// A member-specific reason that prevents a preset apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberRefusal {
    /// Stable item identity.
    pub item_id: String,
    /// Kind stored in the preset.
    pub kind: ItemKind,
    /// Human-readable refusal reason.
    pub reason: String,
}

/// A deleted membership is retained for repairability but excluded from ratios.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedMemberWarning {
    /// Stable item identity no longer present in the item index.
    pub item_id: String,
    /// Stored member kind.
    pub kind: ItemKind,
}

/// A concrete member transition produced by the kind-specific deployment planner.
#[derive(Clone, Debug)]
pub struct MemberTransition {
    /// Item identity changed by this plan.
    pub item_id: String,
    /// Enablement before the transition; `None` only when the item did not exist.
    pub prior_enabled: Option<bool>,
    /// Reviewed filesystem operations, absent for a no-op member.
    pub change_set: Option<ChangeSet>,
}

impl MemberTransition {
    /// Adapts the MCP planner result while preserving its store-before-native ordering.
    pub fn from_mcp_plan(
        item_id: &ItemId,
        prior_enabled: Option<bool>,
        plan: &crate::mcp_plan::McpPlan,
    ) -> Result<Self, PresetServiceError> {
        if let crate::mcp_plan::McpPlanStatus::NotSupported { reason, .. } = &plan.status {
            return Err(PresetServiceError::MemberPlanning(reason.clone()));
        }
        let mut sets = plan.apply_order();
        let change_set = if let Some(first) = sets.next() {
            let mut combined = first.clone();
            for set in sets {
                combined.ops.extend(set.ops.clone());
            }
            combined.validate()?;
            Some(combined)
        } else {
            None
        };
        Ok(Self {
            item_id: item_id.to_string(),
            prior_enabled,
            change_set,
        })
    }

    /// Adapts an existing skill-agent plan without rebuilding its reviewed ChangeSet.
    #[must_use]
    pub fn from_skill_agent_plan(
        item_id: &ItemId,
        prior_enabled: Option<bool>,
        plan: &crate::skill_agent::SkillAgentPlan,
    ) -> Self {
        Self {
            item_id: item_id.to_string(),
            prior_enabled,
            change_set: plan.change_set.clone(),
        }
    }
}

/// In-memory native-file contents used to plan successive members against one file snapshot.
#[derive(Clone, Debug, Default)]
pub struct NativeFileOverlay {
    files: BTreeMap<std::path::PathBuf, Vec<u8>>,
}

impl NativeFileOverlay {
    /// Builds an overlay from the exact initial native-file snapshots read for this operation.
    #[must_use]
    pub fn new(files: impl IntoIterator<Item = (std::path::PathBuf, Vec<u8>)>) -> Self {
        Self {
            files: files.into_iter().collect(),
        }
    }

    /// Returns the current planned bytes, or `None` if the file was initially absent.
    #[must_use]
    pub fn get(&self, path: &std::path::Path) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }

    fn update(&mut self, set: &ChangeSet) {
        for operation in &set.ops {
            match operation {
                at_model::FileOp::Write { path, bytes, .. } => {
                    self.files.insert(path.clone(), bytes.clone());
                }
                at_model::FileOp::Delete { path, .. } => {
                    self.files.remove(path);
                }
                _ => {}
            }
        }
    }
}

/// A complete, preflighted preset operation. It is not yet applied.
#[derive(Clone, Debug)]
pub struct PresetPlan {
    /// Preset whose transitions this plan applies.
    pub preset_id: String,
    /// One combined ChangeSet for the entire preset operation; `None` means no changes.
    pub change_set: Option<ChangeSet>,
    /// Requested target.
    pub target: Target,
    /// Each changed member's prior enablement for application history.
    pub transitions: Vec<MemberTransition>,
    /// Existing-member ratio before planning.
    pub ratio_before: repository::AppliedRatio,
    /// Existing-member ratio after planning.
    pub ratio_after: repository::AppliedRatio,
    /// Deleted rows excluded from both ratios.
    pub deleted_members: Vec<DeletedMemberWarning>,
}

/// Errors from preset CRUD or preflight.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PresetServiceError {
    /// Preset repository or database failure.
    #[error(transparent)]
    Repository(#[from] repository::PresetError),
    /// General database failure.
    #[error(transparent)]
    Database(#[from] DbError),
    /// One or more members prevent the whole apply; no ChangeSet is returned.
    #[error("preset application refused: {0:?}")]
    MembersRefused(Vec<MemberRefusal>),
    /// Caller omitted a transition for an enabled, existing member.
    #[error("member planner did not return a transition for item `{0}`")]
    MissingTransition(String),
    /// A member-specific planner could not produce a safe transition.
    #[error("member planning failed: {0}")]
    MemberPlanning(String),
    /// The target differs from the recorded application destination.
    #[error("unapply target does not match the recorded application target")]
    TargetMismatch,
    /// A component plan is invalid.
    #[error(transparent)]
    ChangeSet(#[from] at_model::ChangeSetError),
    /// The authorized-root apply pipeline failed or rolled back.
    #[error(transparent)]
    Apply(#[from] at_fs::ChangeSetApplyError),
}

/// Preset repository facade and preflight policy.
pub struct PresetService<'a> {
    database: &'a mut Database,
}

impl<'a> PresetService<'a> {
    /// Creates a service over the already-migrated database.
    #[must_use]
    pub const fn new(database: &'a mut Database) -> Self {
        Self { database }
    }

    /// Creates a preset, optionally with initial skill and MCP members.
    ///
    /// # Errors
    /// Returns a typed repository error for invalid or duplicate names and a DB error on failure.
    pub fn create(
        &mut self,
        name: &str,
        description: Option<&str>,
        scope: Scope,
        members: &[(ItemId, ItemKind)],
    ) -> Result<String, PresetServiceError> {
        if members
            .iter()
            .any(|(_, kind)| !matches!(kind, ItemKind::Skill | ItemKind::McpServer))
        {
            return Err(repository::PresetError::InvalidValue {
                field: "kind",
                reason: "presets accept skills and MCP servers only".to_owned(),
            }
            .into());
        }
        // `create_preset` runs its own transaction, so members are added afterwards and the
        // preset is removed again (its members cascade) when any member cannot be recorded.
        let id = repository::create_preset(
            self.database.connection_mut(),
            &repository::NewPreset {
                id: None,
                name,
                description,
                scope,
                is_builtin: false,
            },
        )?;
        for (item_id, kind) in members {
            if let Err(error) =
                repository::set_member(self.database.connection(), &id, item_id, *kind, true)
            {
                let _ = repository::delete_preset(self.database.connection(), &id);
                return Err(error.into());
            }
        }
        Ok(id)
    }

    /// Renames a preset without changing its identity.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<bool, PresetServiceError> {
        Ok(repository::rename_preset(
            self.database.connection_mut(),
            id,
            name,
        )?)
    }

    /// Deletes a preset; its application history is retained by the schema.
    pub fn delete(&mut self, id: &str) -> Result<bool, PresetServiceError> {
        Ok(repository::delete_preset(self.database.connection(), id)?)
    }

    /// Lists presets with repository-computed member counts.
    pub fn list(&self) -> Result<Vec<repository::PresetRecord>, PresetServiceError> {
        Ok(repository::list_presets(self.database.connection())?)
    }

    /// Adds or changes a skill or MCP server member.
    pub fn set_member(
        &mut self,
        preset_id: &str,
        item_id: &ItemId,
        kind: ItemKind,
        enabled: bool,
    ) -> Result<(), PresetServiceError> {
        if !matches!(kind, ItemKind::Skill | ItemKind::McpServer) {
            return Err(repository::PresetError::InvalidValue {
                field: "kind",
                reason: "presets accept skills and MCP servers only".to_owned(),
            }
            .into());
        }
        repository::set_member(
            self.database.connection(),
            preset_id,
            item_id,
            kind,
            enabled,
        )?;
        Ok(())
    }

    /// Removes a member from a preset.
    pub fn remove_member(
        &mut self,
        preset_id: &str,
        item_id: &ItemId,
    ) -> Result<bool, PresetServiceError> {
        Ok(repository::remove_member(
            self.database.connection(),
            preset_id,
            item_id,
        )?)
    }

    /// Returns the repository's applied ratio and an explicit warning for each deleted member.
    pub fn applied_ratio(
        &self,
        preset_id: &str,
        target: &Target,
    ) -> Result<(repository::AppliedRatio, Vec<DeletedMemberWarning>), PresetServiceError> {
        let target_id = target.id();
        let ratio = repository::applied_ratio(
            self.database.connection(),
            preset_id,
            target.kind(),
            target_id.as_ref(),
        )?;
        let warnings = ratio
            .members
            .iter()
            .filter(|member| !member.exists)
            .map(|member| DeletedMemberWarning {
                item_id: member.item_id.clone(),
                kind: member.kind,
            })
            .collect();
        Ok((ratio, warnings))
    }

    /// Plans enabled members sequentially, updating an in-memory overlay after each transition.
    ///
    /// The callback should use `overlay` as the current snapshot when rendering the next skill or
    /// MCP member. This prevents several entries targeting one native document from being
    /// planned against stale bytes. No disk or database mutation is performed.
    pub fn plan_apply_with(
        &self,
        preset_id: &str,
        target: Target,
        mut overlay: NativeFileOverlay,
        mut planner: impl FnMut(
            &repository::PresetMemberRecord,
            &mut NativeFileOverlay,
        ) -> Result<MemberTransition, String>,
    ) -> Result<PresetPlan, PresetServiceError> {
        let members = repository::preset_members(self.database.connection(), preset_id)?;
        let mut transitions = Vec::new();
        for member in members
            .iter()
            .filter(|member| member.enabled && member.exists)
        {
            let transition =
                planner(member, &mut overlay).map_err(PresetServiceError::MemberPlanning)?;
            if transition.item_id != member.item_id {
                return Err(PresetServiceError::MemberPlanning(format!(
                    "planner returned item `{}` for member `{}`",
                    transition.item_id, member.item_id
                )));
            }
            if let Some(change_set) = &transition.change_set {
                overlay.update(change_set);
            }
            transitions.push(transition);
        }
        self.plan_apply(preset_id, target, transitions)
    }

    /// Preflights concrete member transitions and constructs one preset ChangeSet.
    ///
    /// Existing member transitions must include their actual prior enabled state. Already-enabled
    /// members have no ChangeSet; disabled members require one. Deleted or globally locked members
    /// block the entire plan.
    pub fn plan_apply(
        &self,
        preset_id: &str,
        target: Target,
        transitions: Vec<MemberTransition>,
    ) -> Result<PresetPlan, PresetServiceError> {
        let (mut before, deleted) = self.applied_ratio(preset_id, &target)?;
        let member_rows = repository::preset_members(self.database.connection(), preset_id)?;
        let mut refused = Vec::new();
        let mut required = Vec::new();
        for member in member_rows.iter().filter(|member| member.enabled) {
            if !member.exists {
                refused.push(MemberRefusal {
                    item_id: member.item_id.clone(),
                    kind: member.kind,
                    reason: "member item was deleted".to_owned(),
                });
                continue;
            }
            let item_id = ItemId::from_str(&member.item_id)
                .map_err(|error| PresetServiceError::Database(DbError::from(error)))?;
            if let Some(lock) =
                at_db::locks::find_lock(self.database.connection(), &item_id, Scope::Global)?
            {
                refused.push(MemberRefusal {
                    item_id: member.item_id.clone(),
                    kind: member.kind,
                    reason: format!(
                        "locked: {}",
                        lock.reason.as_deref().unwrap_or("locked at Global scope")
                    ),
                });
            }
            required.push(member.item_id.as_str());
        }
        if !refused.is_empty() {
            return Err(PresetServiceError::MembersRefused(refused));
        }
        let mut supplied = transitions;
        for item in &required {
            if !supplied
                .iter()
                .any(|transition| transition.item_id == *item)
            {
                return Err(PresetServiceError::MissingTransition((*item).to_owned()));
            }
        }
        supplied.retain(|transition| required.contains(&transition.item_id.as_str()));
        for transition in &supplied {
            match (transition.prior_enabled, transition.change_set.is_some()) {
                (None, _) => {
                    return Err(PresetServiceError::MemberPlanning(format!(
                        "member `{}` has no prior enablement value",
                        transition.item_id
                    )));
                }
                (Some(true), true) => {
                    return Err(PresetServiceError::MemberPlanning(format!(
                        "already-enabled member `{}` has a write plan",
                        transition.item_id
                    )));
                }
                (Some(false), false) => {
                    return Err(PresetServiceError::MemberPlanning(format!(
                        "disabled member `{}` has no write plan",
                        transition.item_id
                    )));
                }
                _ => {}
            }
        }
        for member in &mut before.members {
            if let Some(transition) = supplied
                .iter()
                .find(|transition| transition.item_id == member.item_id)
            {
                member.enabled = transition.prior_enabled.unwrap_or(false);
            }
        }
        recompute_ratio(&mut before);
        let combined = combine_change_sets(&supplied, "preset_apply")?;
        let mut after = before.clone();
        for member in &mut after.members {
            if member.exists
                && supplied
                    .iter()
                    .any(|transition| transition.item_id == member.item_id)
            {
                member.enabled = true;
            }
        }
        after.enabled = after
            .members
            .iter()
            .filter(|member| member.exists && member.enabled)
            .count() as i64;
        after.total = after.members.iter().filter(|member| member.exists).count() as i64;
        after.complete = after.total > 0 && after.enabled == after.total;
        Ok(PresetPlan {
            preset_id: preset_id.to_owned(),
            change_set: combined,
            target,
            transitions: supplied,
            ratio_before: before,
            ratio_after: after,
            deleted_members: deleted,
        })
    }

    /// Records a successfully applied ChangeSet and each changed member's previous state.
    ///
    /// Call only after `at-fs` returns a successful `ApplyOutcome`. Application history uses the
    /// operation id emitted by that pipeline, preserving the single operation identity.
    pub fn record_apply(
        &mut self,
        preset_id: &str,
        target: &Target,
        outcome: &at_fs::changeset::ApplyOutcome,
        transitions: &[MemberTransition],
    ) -> Result<i64, PresetServiceError> {
        let members = transitions
            .iter()
            .filter(|transition| transition.change_set.is_some())
            .map(|transition| repository::ApplicationMemberSpec {
                item_id: transition.item_id.as_str(),
                prior_enabled: transition.prior_enabled,
            })
            .collect();
        let target_id = target.id();
        Ok(repository::record_application(
            self.database.connection_mut(),
            &repository::NewApplication {
                preset_id,
                target_kind: target.kind(),
                target_id: target_id.as_ref(),
                operation_id: Some(&outcome.operation_id),
                members,
                member_count: None,
            },
        )?)
    }

    /// Plans unapply from a recorded application, restoring each member's exact prior state.
    ///
    /// `inverse_sets` are produced by the skill and MCP planners. `prior_enabled` in returned
    /// transitions always comes from `preset_application_members`, not caller input.
    pub fn plan_unapply(
        &self,
        application_id: i64,
        target: Target,
        inverse_sets: Vec<MemberTransition>,
    ) -> Result<PresetPlan, PresetServiceError> {
        let preset_id = self.application_preset_id(application_id)?;
        let (stored_kind, stored_target) = self
            .database
            .connection()
            .query_row(
                "SELECT target_kind, target_id FROM preset_applications WHERE id = ?1",
                [application_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(DbError::from)?;
        let target_id = target.id();
        if stored_kind != target.kind().as_str() || stored_target != target_id.as_ref() {
            return Err(PresetServiceError::TargetMismatch);
        }
        let history = repository::application_members(self.database.connection(), application_id)?;
        let members = repository::preset_members(self.database.connection(), &preset_id)?;
        let mut refused = Vec::new();
        let mut transitions = Vec::with_capacity(history.len());
        for prior in history {
            let item_id = ItemId::from_str(&prior.item_id)
                .map_err(|error| PresetServiceError::Database(DbError::from(error)))?;
            let Some(member) = members
                .iter()
                .find(|member| member.item_id == prior.item_id && member.exists)
            else {
                refused.push(MemberRefusal {
                    item_id: prior.item_id.clone(),
                    kind: members
                        .iter()
                        .find(|member| member.item_id == prior.item_id)
                        .map_or(ItemKind::Skill, |member| member.kind),
                    reason: "member item was deleted since the application".to_owned(),
                });
                continue;
            };
            if let Some(lock) =
                at_db::locks::find_lock(self.database.connection(), &item_id, Scope::Global)?
            {
                refused.push(MemberRefusal {
                    item_id: prior.item_id.clone(),
                    kind: member.kind,
                    reason: format!(
                        "locked: {}",
                        lock.reason.as_deref().unwrap_or("locked at Global scope")
                    ),
                });
                continue;
            }
            let Some(mut transition) = inverse_sets
                .iter()
                .find(|transition| transition.item_id == prior.item_id)
                .cloned()
            else {
                return Err(PresetServiceError::MissingTransition(prior.item_id));
            };
            transition.prior_enabled = prior.prior_enabled;
            transitions.push(transition);
        }
        if !refused.is_empty() {
            return Err(PresetServiceError::MembersRefused(refused));
        }
        let change_set = combine_change_sets(&transitions, "preset_unapply")?;
        let (mut ratio_before, deleted_members) = self.applied_ratio(&preset_id, &target)?;
        for member in &mut ratio_before.members {
            if transitions
                .iter()
                .any(|transition| transition.item_id == member.item_id)
            {
                member.enabled = true;
            }
        }
        recompute_ratio(&mut ratio_before);
        let mut ratio_after = ratio_before.clone();
        for member in &mut ratio_after.members {
            if let Some(transition) = transitions
                .iter()
                .find(|transition| transition.item_id == member.item_id)
            {
                member.enabled = transition.prior_enabled.unwrap_or(false);
            }
        }
        ratio_after.enabled = ratio_after
            .members
            .iter()
            .filter(|member| member.exists && member.enabled)
            .count() as i64;
        ratio_after.total = ratio_after
            .members
            .iter()
            .filter(|member| member.exists)
            .count() as i64;
        ratio_after.complete = ratio_after.total > 0 && ratio_after.enabled == ratio_after.total;
        Ok(PresetPlan {
            preset_id,
            change_set,
            target,
            transitions,
            ratio_before,
            ratio_after,
            deleted_members,
        })
    }

    /// Applies the exact planned ChangeSet through the authorized-root pipeline, then records
    /// prior values. Pass the same set that was shown in the user's review; do not rebuild it.
    ///
    /// `roots` must be the AppPaths-derived native roots authorized for this operation.
    pub fn apply_plan(
        &mut self,
        plan: &PresetPlan,
        context: &mut at_fs::changeset::ApplyContext<'_>,
        roots: &at_fs::changeset::ApplyRoots,
    ) -> Result<at_fs::changeset::ApplyOutcome, PresetServiceError> {
        let set = plan
            .change_set
            .as_ref()
            .ok_or_else(|| PresetServiceError::MemberPlanning("no changes to apply".to_owned()))?;
        let outcome = at_fs::changeset::apply_with_roots(set, context, roots)?;
        if set.operation_kind.as_str() == "preset_apply" {
            self.record_apply(&plan.preset_id, &plan.target, &outcome, &plan.transitions)?;
        }
        Ok(outcome)
    }

    fn application_preset_id(&self, application_id: i64) -> Result<String, PresetServiceError> {
        self.database
            .connection()
            .query_row(
                "SELECT preset_id FROM preset_applications WHERE id = ?1",
                [application_id],
                |row| row.get(0),
            )
            .map_err(|error| PresetServiceError::Database(DbError::from(error)))
    }
}

fn combine_change_sets(
    transitions: &[MemberTransition],
    operation_kind: &str,
) -> Result<Option<ChangeSet>, PresetServiceError> {
    let mut sets = transitions
        .iter()
        .filter_map(|transition| transition.change_set.as_ref());
    let Some(first) = sets.next() else {
        return Ok(None);
    };
    let mut combined = first.clone();
    combined.id = uuid::Uuid::now_v7().to_string();
    combined.preview_id = combined.id.clone();
    combined.operation_kind = OperationKind::parse(operation_kind)?;
    combined.summary = if operation_kind == "preset_unapply" {
        "Unapply preset".to_owned()
    } else {
        "Apply preset".to_owned()
    };
    for set in sets {
        for operation in &set.ops {
            if let at_model::FileOp::Write {
                path,
                bytes,
                backup,
                ..
            } = operation
                && let Some(at_model::FileOp::Write {
                    bytes: previous_bytes,
                    backup: previous_backup,
                    ..
                }) = combined.ops.iter_mut().find(
                    |existing| matches!(existing, at_model::FileOp::Write { path: old, .. } if old == path),
                )
            {
                *previous_bytes = bytes.clone();
                *previous_backup |= *backup;
                continue;
            }
            combined.ops.push(operation.clone());
        }
    }
    combined.validate()?;
    Ok(Some(combined))
}

fn recompute_ratio(ratio: &mut repository::AppliedRatio) {
    ratio.enabled = ratio
        .members
        .iter()
        .filter(|member| member.exists && member.enabled)
        .count() as i64;
    ratio.total = ratio.members.iter().filter(|member| member.exists).count() as i64;
    ratio.complete = ratio.total > 0 && ratio.enabled == ratio.total;
}

#[cfg(test)]
mod tests {
    use super::*;
    use at_model::{ExpectedState, FileOp};
    use std::path::PathBuf;

    fn transition(path: &str) -> MemberTransition {
        MemberTransition {
            item_id: "itm_0192a9b2-1f74-7bc1-8b30-12fc23f91930".to_owned(),
            prior_enabled: Some(false),
            change_set: Some(ChangeSet {
                id: "member-plan".to_owned(),
                preview_id: "member-preview".to_owned(),
                operation_kind: OperationKind::parse("deploy").expect("known kind"),
                scope: Some(Scope::Project),
                project_id: Some("project-1".to_owned()),
                summary: "Enable member".to_owned(),
                ops: vec![FileOp::Write {
                    path: PathBuf::from(path),
                    bytes: path.as_bytes().to_vec(),
                    expected: ExpectedState::absent(),
                    follow_symlink: false,
                    backup: false,
                }],
                created_at_unix_seconds: 1,
            }),
        }
    }

    #[test]
    fn target_kind_and_id_match_the_requested_destination() {
        let agent = Target::Agent(AgentId::Codex);
        let repo = Target::Repo("project-1".to_owned());
        assert_eq!(agent.kind(), repository::PresetTargetKind::Agent);
        assert_eq!(agent.id().as_ref(), "codex");
        assert_eq!(repo.kind(), repository::PresetTargetKind::Project);
        assert_eq!(repo.id(), "project-1");
    }

    #[test]
    fn create_manage_and_delete_preset_members() {
        let mut database = Database::open_in_memory().expect("database");
        let item = ItemId::new();
        let id = {
            let mut service = PresetService::new(&mut database);
            let id = service
                .create(
                    "Developer setup",
                    Some("Work tools"),
                    Scope::User,
                    &[
                        (item, ItemKind::Skill),
                        (ItemId::new(), ItemKind::McpServer),
                    ],
                )
                .expect("create");
            let listed = service.list().expect("list");
            assert_eq!(listed[0].member_count, 2);
            assert!(service.rename(&id, "Engineering").expect("rename"));
            assert!(service.remove_member(&id, &item).expect("remove member"));
            assert_eq!(service.list().expect("list")[0].member_count, 1);
            id
        };
        let mut service = PresetService::new(&mut database);
        assert!(service.delete(&id).expect("delete"));
        assert!(service.list().expect("list").is_empty());
    }

    #[test]
    fn unapply_uses_the_recorded_prior_value_and_target() {
        let mut database = Database::open_in_memory().expect("database");
        let item = ItemId::new();
        database
            .connection()
            .execute(
                "INSERT INTO items (id, kind, scope, slug, name) \
                 VALUES (?1, 'skill', 'user', 'undo-skill', 'Undo skill')",
                [item.to_string()],
            )
            .expect("item");
        let preset_id = {
            let mut service = PresetService::new(&mut database);
            service
                .create(
                    "Unapply setup",
                    None,
                    Scope::User,
                    &[(item, ItemKind::Skill)],
                )
                .expect("preset")
        };
        let item_text = item.to_string();
        let application_id = at_db::presets::record_application(
            database.connection_mut(),
            &at_db::presets::NewApplication {
                preset_id: &preset_id,
                target_kind: repository::PresetTargetKind::Agent,
                target_id: "codex",
                operation_id: None,
                members: vec![repository::ApplicationMemberSpec {
                    item_id: &item_text,
                    prior_enabled: Some(false),
                }],
                member_count: None,
            },
        )
        .expect("application history");
        let mut inverse = transition("/store/undo-skill");
        inverse.item_id = item.to_string();
        let service = PresetService::new(&mut database);
        let plan = service
            .plan_unapply(application_id, Target::Agent(AgentId::Codex), vec![inverse])
            .expect("unapply plan");
        assert_eq!(
            plan.change_set
                .as_ref()
                .expect("change set")
                .operation_kind
                .as_str(),
            "preset_unapply"
        );
        assert_eq!(plan.transitions[0].prior_enabled, Some(false));
        assert_eq!(
            (plan.ratio_before.enabled, plan.ratio_after.enabled),
            (1, 0)
        );
    }
    #[test]
    fn deleted_members_are_excluded_and_block_application_with_warning_reason() {
        let mut database = Database::open_in_memory().expect("database");
        let id = {
            let mut service = PresetService::new(&mut database);
            service
                .create(
                    "Stale setup",
                    None,
                    Scope::User,
                    &[(ItemId::new(), ItemKind::Skill)],
                )
                .expect("create")
        };
        let service = PresetService::new(&mut database);
        let target = Target::Agent(AgentId::Codex);
        let (ratio, warnings) = service.applied_ratio(&id, &target).expect("ratio");
        assert_eq!((ratio.enabled, ratio.total), (0, 0));
        assert_eq!(warnings.len(), 1);
        let error = service
            .plan_apply(&id, target, Vec::new())
            .expect_err("deleted members block apply");
        let PresetServiceError::MembersRefused(members) = error else {
            panic!("expected member refusal");
        };
        assert_eq!(members.len(), 1);
        assert!(members[0].reason.contains("deleted"));
    }
    #[test]
    fn apply_combines_member_changes_under_one_preset_operation() {
        let plan = combine_change_sets(
            &[transition("/store/shared"), transition("/store/shared")],
            "preset_apply",
        )
        .expect("combine")
        .expect("one operation");
        assert_eq!(plan.operation_kind.as_str(), "preset_apply");
        assert_eq!(plan.ops.len(), 1, "shared native files get one final write");
        let FileOp::Write {
            bytes, expected, ..
        } = &plan.ops[0]
        else {
            panic!("expected write")
        };
        assert_eq!(bytes, b"/store/shared");
        assert_eq!(
            expected,
            &ExpectedState::absent(),
            "retain original expectation"
        );
        assert_eq!(plan.summary, "Apply preset");
    }

    #[test]
    fn member_planners_observe_shared_file_updates_in_order() {
        let mut database = Database::open_in_memory().expect("database");
        let first = ItemId::new();
        let second = ItemId::new();
        for (item, slug) in [(first, "first"), (second, "second")] {
            database
                .connection()
                .execute(
                    "INSERT INTO items (id, kind, scope, slug, name) \
                     VALUES (?1, 'skill', 'user', ?2, ?2)",
                    rusqlite::params![item.to_string(), slug],
                )
                .expect("item");
        }
        let preset_id = {
            let mut service = PresetService::new(&mut database);
            service
                .create(
                    "Shared file",
                    None,
                    Scope::User,
                    &[(first, ItemKind::Skill), (second, ItemKind::Skill)],
                )
                .expect("preset")
        };
        let service = PresetService::new(&mut database);
        let path = PathBuf::from("/fixture/agent-config.json");
        let mut observed = Vec::new();
        let plan = service
            .plan_apply_with(
                &preset_id,
                Target::Agent(AgentId::ClaudeCode),
                NativeFileOverlay::new([(path.clone(), b"initial".to_vec())]),
                |member, overlay| {
                    let previous = overlay.get(&path).expect("snapshot");
                    observed.push(previous.to_vec());
                    let bytes = [previous, member.item_id.as_bytes()].concat();
                    Ok(MemberTransition {
                        item_id: member.item_id.clone(),
                        prior_enabled: Some(false),
                        change_set: Some(ChangeSet {
                            id: member.item_id.clone(),
                            preview_id: member.item_id.clone(),
                            operation_kind: OperationKind::parse("edit_mcp")
                                .expect("known operation"),
                            scope: Some(Scope::User),
                            project_id: None,
                            summary: "Update MCP config".to_owned(),
                            ops: vec![FileOp::Write {
                                path: path.clone(),
                                bytes,
                                expected: ExpectedState::hash(at_fs::sha256_hex(previous))
                                    .expect("hash"),
                                follow_symlink: false,
                                backup: true,
                            }],
                            created_at_unix_seconds: 1,
                        }),
                    })
                },
            )
            .expect("plan");
        assert_eq!(observed[0], b"initial");
        assert!(observed[1].starts_with(b"initial"));
        let set = plan.change_set.expect("combined change set");
        assert_eq!(set.ops.len(), 1);
        let FileOp::Write {
            bytes, expected, ..
        } = &set.ops[0]
        else {
            panic!("expected merged native-file write")
        };
        assert!(bytes.starts_with(b"initial"));
        assert_eq!(
            expected,
            &ExpectedState::hash(at_fs::sha256_hex(b"initial")).expect("initial hash")
        );
    }
    #[test]
    fn empty_transition_set_is_a_no_op_without_an_operation() {
        assert!(
            combine_change_sets(&[], "preset_apply")
                .expect("empty plan")
                .is_none()
        );
    }
}
