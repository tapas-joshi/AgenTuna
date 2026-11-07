//! Pure state machine for the add-skill wizard.
//!
//! Provider work is represented as effects; this module never performs filesystem or network IO.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

/// Ordered wizard step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WizardStep {
    /// Choose an import source.
    Source,
    /// Configure source-specific details.
    Details,
    /// Select destination scope and agents.
    ScopeAgents,
    /// Review the proposed import.
    Review,
    /// Import completed.
    Done,
}

/// Supported skill sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceKind {
    /// Create from a built-in template.
    New,
    /// Clone a Git repository.
    Git,
    /// Copy from a registered AgenTuna library.
    Library,
    /// Install from a marketplace listing.
    Marketplace,
    /// Import a local directory.
    Folder,
    /// Import a ZIP archive.
    Zip,
}

/// How selected skills are brought into the destination library.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[non_exhaustive]
pub enum LibraryTransferMode {
    /// Copy each source package into the destination library.
    #[default]
    Copy,
    /// Link each destination entry to its source package.
    Link,
}

/// The current source-specific details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct SourceDetails {
    /// Git repository URL.
    pub repo_url: String,
    /// Optional Git branch, tag or commit.
    pub repo_reference: String,
    /// Selected library skill identifiers, which may span multiple sources.
    pub library_selections: Vec<String>,
    /// Whether library packages are copied or linked into the destination.
    pub library_mode: LibraryTransferMode,
    /// Marketplace skill identifier.
    pub marketplace_id: String,
    /// Folder path selected by the user.
    pub folder_path: String,
    /// ZIP path selected by the user.
    pub zip_path: String,
    /// Use-when description supplied for a new skill.
    pub description: String,
    /// Selected template identifier for a new skill.
    pub template: Option<String>,
    /// Selected folder skill candidates relative to the imported directory.
    pub folder_selections: Vec<String>,
    /// Selected ZIP skill candidates relative to the extracted archive.
    pub zip_selections: Vec<String>,
    /// Selected imported repository skill identifiers.
    pub fetched_repo_selections: Vec<String>,
}

/// Current complete wizard draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
pub struct AddSkillWizard {
    /// Current step.
    pub step: WizardStep,
    /// Selected source type.
    pub source: Option<SourceKind>,
    /// Source fields, preserved while navigating.
    pub details: SourceDetails,
    /// User-visible skill name.
    pub name: String,
    /// Destination scope identifier.
    pub scope: Option<String>,
    /// Selected agent identifiers.
    pub agents: Vec<String>,
    /// Whether an operation has been submitted and completed.
    pub completed: bool,
}

impl Default for AddSkillWizard {
    fn default() -> Self {
        Self {
            source: None,
            step: WizardStep::Source,
            details: SourceDetails::default(),
            name: String::new(),
            scope: None,
            agents: Vec::new(),
            completed: false,
        }
    }
}

/// User or provider event consumed by the state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WizardEvent {
    /// Select an import source.
    SelectSource(SourceKind),
    /// Update the proposed skill name.
    SetName(String),
    /// Update a typed source detail.
    SetDetails(Box<SourceDetails>),
    /// Select the destination scope.
    SetScope(String),
    /// Replace selected agent identifiers.
    SetAgents(Vec<String>),
    /// Explicitly ask an injected provider to fetch or clone source data.
    FetchSource,
    /// Provider reports that source candidates are available.
    SourceFetched {
        /// Candidate identifiers returned by the injected source provider.
        selections: Vec<String>,
    },
    /// Advance only when the current step validates.
    Next,
    /// Return one step while preserving the draft.
    Back,
    /// Request cancel; dirty drafts require explicit discard confirmation.
    Cancel,
    /// Confirm discarding a dirty draft.
    ConfirmDiscard,
    /// The caller confirms the reviewed import completed successfully.
    ImportCompleted,
}

/// Side effect requested from the caller; no effect performs IO by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WizardEffect {
    /// Run the selected source provider after explicit user action.
    FetchSource {
        /// Source whose injected provider should be invoked.
        source: SourceKind,
        /// Current validated source parameters.
        details: Box<SourceDetails>,
    },
    /// Discard the current draft and close the wizard.
    DiscardDraft,
    /// Show a confirmation prompt before discarding.
    ConfirmDiscard,
    /// Caller may start the reviewed import operation.
    ReadyToImport,
    /// The current step failed validation and cannot advance.
    ValidationFailed {
        /// Why the current step cannot advance.
        issue: WizardValidationError,
    },
}

/// Validation issue for the current step.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../ui/src/generated/")]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WizardValidationError {
    /// A source has not been selected.
    #[error("select a skill source")]
    SourceRequired,
    /// Required source-specific detail is missing or malformed.
    #[error("source details are incomplete or invalid")]
    DetailsRequired,
    /// A proposed skill name is empty.
    #[error("enter a skill name")]
    NameRequired,
    /// A destination scope is missing.
    #[error("select a destination scope")]
    ScopeRequired,
    /// At least one agent must be selected.
    #[error("select at least one agent")]
    AgentRequired,
    /// A source selection has not been made from the provider results.
    #[error("select an imported skill")]
    ImportedSkillRequired,
}

impl AddSkillWizard {
    /// Whether user-editable draft values differ from a clean draft.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.source.is_some()
            || self.details != SourceDetails::default()
            || !self.name.is_empty()
            || self.scope.is_some()
            || !self.agents.is_empty()
            || self.completed
    }

    /// Applies one event and returns the requested external effects.
    pub fn dispatch(&mut self, event: WizardEvent) -> Vec<WizardEffect> {
        match event {
            WizardEvent::SelectSource(source) if self.step == WizardStep::Source => {
                self.source = Some(source);
                self.details = SourceDetails::default();
                Vec::new()
            }
            WizardEvent::SetName(name) => {
                self.name = name;
                Vec::new()
            }
            WizardEvent::SetDetails(details) => {
                self.details = *details;
                Vec::new()
            }
            WizardEvent::SetScope(scope) => {
                self.scope = Some(scope);
                Vec::new()
            }
            WizardEvent::SetAgents(agents) => {
                self.agents = agents;
                Vec::new()
            }
            WizardEvent::FetchSource => self.source.as_ref().map_or_else(Vec::new, |source| {
                if matches!(
                    source,
                    SourceKind::Git
                        | SourceKind::Marketplace
                        | SourceKind::Library
                        | SourceKind::Folder
                        | SourceKind::Zip
                ) {
                    vec![WizardEffect::FetchSource {
                        source: source.clone(),
                        details: Box::new(self.details.clone()),
                    }]
                } else {
                    Vec::new()
                }
            }),
            WizardEvent::SourceFetched { selections } => {
                self.details.fetched_repo_selections = selections;
                Vec::new()
            }
            WizardEvent::Next => {
                if let Err(issue) = self.validate_current() {
                    return vec![WizardEffect::ValidationFailed { issue }];
                }
                self.step = match self.step {
                    WizardStep::Source => WizardStep::Details,
                    WizardStep::Details => WizardStep::ScopeAgents,
                    WizardStep::ScopeAgents => WizardStep::Review,
                    WizardStep::Review => {
                        return vec![WizardEffect::ReadyToImport];
                    }
                    WizardStep::Done => WizardStep::Done,
                };
                Vec::new()
            }
            WizardEvent::Back => {
                self.step = match self.step {
                    WizardStep::Source => WizardStep::Source,
                    WizardStep::Details => WizardStep::Source,
                    WizardStep::ScopeAgents => WizardStep::Details,
                    WizardStep::Review => WizardStep::ScopeAgents,
                    WizardStep::Done => WizardStep::Done,
                };
                Vec::new()
            }
            WizardEvent::Cancel => {
                if self.is_dirty() {
                    vec![WizardEffect::ConfirmDiscard]
                } else {
                    vec![WizardEffect::DiscardDraft]
                }
            }
            WizardEvent::ConfirmDiscard => vec![WizardEffect::DiscardDraft],
            WizardEvent::ImportCompleted if self.step == WizardStep::Review => {
                self.completed = true;
                self.step = WizardStep::Done;
                Vec::new()
            }
            WizardEvent::ImportCompleted | WizardEvent::SelectSource(_) => Vec::new(),
        }
    }

    /// Validates required data for the current step.
    pub fn validate_current(&self) -> Result<(), WizardValidationError> {
        match self.step {
            WizardStep::Source => self
                .source
                .as_ref()
                .map(|_| ())
                .ok_or(WizardValidationError::SourceRequired),
            WizardStep::Details => {
                let source = self
                    .source
                    .as_ref()
                    .ok_or(WizardValidationError::SourceRequired)?;
                validate_details(source, &self.details)?;
                if requires_user_name(source) && self.name.trim().is_empty() {
                    Err(WizardValidationError::NameRequired)
                } else {
                    Ok(())
                }
            }
            WizardStep::ScopeAgents => {
                if self.scope.as_deref().is_none_or(str::is_empty) {
                    return Err(WizardValidationError::ScopeRequired);
                }
                if self.agents.is_empty() {
                    return Err(WizardValidationError::AgentRequired);
                }
                Ok(())
            }
            WizardStep::Review => {
                let source = self
                    .source
                    .as_ref()
                    .ok_or(WizardValidationError::SourceRequired)?;
                validate_details(source, &self.details)?;
                if requires_user_name(source) && self.name.trim().is_empty() {
                    return Err(WizardValidationError::NameRequired);
                }
                if self.scope.as_deref().is_none_or(str::is_empty) {
                    return Err(WizardValidationError::ScopeRequired);
                }
                if self.agents.is_empty() {
                    return Err(WizardValidationError::AgentRequired);
                }
                Ok(())
            }
            WizardStep::Done if self.completed => Ok(()),
            WizardStep::Done => Err(WizardValidationError::DetailsRequired),
        }
    }
}

fn validate_details(
    source: &SourceKind,
    details: &SourceDetails,
) -> Result<(), WizardValidationError> {
    let complete = match source {
        SourceKind::New => details
            .template
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty()),
        SourceKind::Git => {
            valid_repository_url(&details.repo_url) && !details.fetched_repo_selections.is_empty()
        }
        SourceKind::Library => !details.library_selections.is_empty(),
        SourceKind::Marketplace => {
            details.marketplace_id.contains('/')
                && details
                    .marketplace_id
                    .split('/')
                    .all(|part| !part.is_empty())
        }
        SourceKind::Folder => {
            !details.folder_path.trim().is_empty() && !details.folder_selections.is_empty()
        }
        SourceKind::Zip => {
            !details.zip_path.trim().is_empty() && !details.zip_selections.is_empty()
        }
    };
    if complete {
        Ok(())
    } else {
        Err(WizardValidationError::DetailsRequired)
    }
}

fn requires_user_name(source: &SourceKind) -> bool {
    matches!(source, SourceKind::New | SourceKind::Marketplace)
}

fn valid_repository_url(value: &str) -> bool {
    let value = value.trim();
    (value.starts_with("https://") || value.starts_with("ssh://") || value.starts_with("git@"))
        && value.len() > 8
        && !value.chars().any(char::is_whitespace)
        && !at_git::import::url_has_credentials(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_details(wizard: &mut AddSkillWizard) {
        wizard.dispatch(WizardEvent::SelectSource(SourceKind::Git));
        wizard.dispatch(WizardEvent::SetName("Example Skill".to_owned()));
        wizard.dispatch(WizardEvent::SetDetails(Box::new(SourceDetails {
            repo_url: "https://example.invalid/skills.git".to_owned(),
            fetched_repo_selections: vec!["repo/skill".to_owned()],
            ..SourceDetails::default()
        })));
    }

    #[test]
    fn repository_urls_with_embedded_credentials_are_rejected() {
        assert!(!valid_repository_url(
            "https://user:secret@example.invalid/skills.git"
        ));
        assert!(!valid_repository_url(
            "https://token@example.invalid/skills.git"
        ));
        assert!(!valid_repository_url(
            "ssh://git@example.invalid/skills.git"
        ));
        assert!(valid_repository_url("https://example.invalid/skills.git"));
        assert!(valid_repository_url("ssh://example.invalid/skills.git"));
        assert!(valid_repository_url("git@example.invalid:org/skills.git"));
    }

    #[test]
    fn gates_navigation_and_preserves_draft_on_back() {
        let mut wizard = AddSkillWizard::default();
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::SourceRequired,
            }]
        );
        complete_details(&mut wizard);
        wizard.dispatch(WizardEvent::Next);
        assert_eq!(wizard.step, WizardStep::Details);
        wizard.dispatch(WizardEvent::Next);
        wizard.dispatch(WizardEvent::SetScope("user".to_owned()));
        wizard.dispatch(WizardEvent::SetAgents(vec!["codex".to_owned()]));
        wizard.dispatch(WizardEvent::Next);
        wizard.dispatch(WizardEvent::Back);
        assert_eq!(wizard.step, WizardStep::ScopeAgents);
        wizard.dispatch(WizardEvent::Back);
        assert_eq!(wizard.step, WizardStep::Details);
        assert_eq!(wizard.name, "Example Skill");
        assert_eq!(wizard.details.fetched_repo_selections, ["repo/skill"]);
        assert_eq!(wizard.agents, ["codex"]);
    }

    #[test]
    fn review_emits_import_effect_and_done_requires_success_event() {
        let mut wizard = AddSkillWizard::default();
        complete_details(&mut wizard);
        wizard.dispatch(WizardEvent::Next);
        wizard.dispatch(WizardEvent::Next);
        wizard.dispatch(WizardEvent::SetScope("user".to_owned()));
        wizard.dispatch(WizardEvent::SetAgents(vec!["codex".to_owned()]));
        wizard.dispatch(WizardEvent::Next);
        assert_eq!(wizard.step, WizardStep::Review);
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ReadyToImport]
        );
        assert_eq!(wizard.step, WizardStep::Review);
        wizard.dispatch(WizardEvent::ImportCompleted);
        assert_eq!(wizard.step, WizardStep::Done);
        assert!(wizard.completed);
    }

    #[test]
    fn cancel_and_fetch_are_explicit_effects() {
        let mut wizard = AddSkillWizard::default();
        assert_eq!(
            wizard.dispatch(WizardEvent::Cancel),
            [WizardEffect::DiscardDraft]
        );
        wizard.dispatch(WizardEvent::SelectSource(SourceKind::Git));
        assert_eq!(
            wizard.dispatch(WizardEvent::Cancel),
            [WizardEffect::ConfirmDiscard]
        );
        assert_eq!(
            wizard.dispatch(WizardEvent::FetchSource),
            [WizardEffect::FetchSource {
                source: SourceKind::Git,
                details: Box::new(SourceDetails::default()),
            }]
        );
        assert_eq!(
            wizard.dispatch(WizardEvent::ConfirmDiscard),
            [WizardEffect::DiscardDraft]
        );
    }

    #[test]
    fn review_back_to_details_then_forward_retains_the_draft() {
        let mut wizard = AddSkillWizard::default();
        wizard.dispatch(WizardEvent::SelectSource(SourceKind::Git));
        wizard.dispatch(WizardEvent::SetName("Example Skill".to_owned()));
        wizard.dispatch(WizardEvent::SetDetails(Box::new(SourceDetails {
            repo_url: "https://example.invalid/skills.git".to_owned(),
            template: Some("basic".to_owned()),
            fetched_repo_selections: vec!["repo/skill".to_owned()],
            ..SourceDetails::default()
        })));
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::Details);
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::ScopeAgents);
        wizard.dispatch(WizardEvent::SetScope("user".to_owned()));
        wizard.dispatch(WizardEvent::SetAgents(vec![
            "codex".to_owned(),
            "omp".to_owned(),
        ]));
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::Review);

        // Back from Review through Scope & agents to Details, then forward again.
        wizard.dispatch(WizardEvent::Back);
        assert_eq!(wizard.step, WizardStep::ScopeAgents);
        wizard.dispatch(WizardEvent::Back);
        assert_eq!(wizard.step, WizardStep::Details);
        assert_eq!(wizard.name, "Example Skill");
        assert_eq!(wizard.details.template.as_deref(), Some("basic"));
        assert_eq!(wizard.details.fetched_repo_selections, ["repo/skill"]);
        assert_eq!(wizard.agents, ["codex", "omp"]);
        assert_eq!(wizard.scope.as_deref(), Some("user"));

        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::ScopeAgents);
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::Review);
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ReadyToImport]
        );
        assert_eq!(wizard.step, WizardStep::Review, "Done requires success");
        wizard.dispatch(WizardEvent::ImportCompleted);
        assert_eq!(wizard.step, WizardStep::Done);
        assert!(wizard.completed);
    }

    fn assert_caller_only(effects: &[WizardEffect]) {
        // Every returned effect is a request the caller performs: the machine
        // itself has no file or network handle and performs no IO.
        for effect in effects {
            match effect {
                WizardEffect::FetchSource { .. }
                | WizardEffect::DiscardDraft
                | WizardEffect::ConfirmDiscard
                | WizardEffect::ReadyToImport
                | WizardEffect::ValidationFailed { .. } => {}
            }
        }
    }

    #[test]
    fn every_transition_is_gated_and_returns_only_caller_effects() {
        let mut wizard = AddSkillWizard::default();
        assert_eq!(wizard.step, WizardStep::Source);
        assert!(!wizard.is_dirty());
        assert_caller_only(&wizard.dispatch(WizardEvent::Cancel));
        assert_eq!(
            wizard.dispatch(WizardEvent::Cancel),
            [WizardEffect::DiscardDraft]
        );
        assert!(wizard.dispatch(WizardEvent::Back).is_empty());
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::SourceRequired,
            }]
        );
        assert_eq!(wizard.step, WizardStep::Source);

        wizard.dispatch(WizardEvent::SelectSource(SourceKind::New));
        assert_eq!(wizard.details, SourceDetails::default());
        assert!(
            wizard.dispatch(WizardEvent::FetchSource).is_empty(),
            "a template source has no provider to fetch"
        );
        assert_eq!(
            wizard.dispatch(WizardEvent::Cancel),
            [WizardEffect::ConfirmDiscard]
        );
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::Details);
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::DetailsRequired,
            }]
        );
        assert_eq!(wizard.step, WizardStep::Details);

        wizard.dispatch(WizardEvent::SetDetails(Box::new(SourceDetails {
            template: Some("basic".to_owned()),
            ..SourceDetails::default()
        })));
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::NameRequired,
            }],
            "a template skill needs a user-visible name"
        );
        wizard.dispatch(WizardEvent::SetName("Templated Skill".to_owned()));
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::ScopeAgents);
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::ScopeRequired,
            }]
        );
        wizard.dispatch(WizardEvent::SetScope("user".to_owned()));
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ValidationFailed {
                issue: WizardValidationError::AgentRequired,
            }]
        );
        wizard.dispatch(WizardEvent::SetAgents(vec!["codex".to_owned()]));
        assert!(wizard.dispatch(WizardEvent::Next).is_empty());
        assert_eq!(wizard.step, WizardStep::Review);
        assert_eq!(
            wizard.dispatch(WizardEvent::Next),
            [WizardEffect::ReadyToImport]
        );
        wizard.dispatch(WizardEvent::ImportCompleted);
        assert_eq!(wizard.step, WizardStep::Done);
        assert_eq!(
            wizard.dispatch(WizardEvent::Cancel),
            [WizardEffect::ConfirmDiscard]
        );
        assert_eq!(
            wizard.dispatch(WizardEvent::ConfirmDiscard),
            [WizardEffect::DiscardDraft]
        );

        let mut git = AddSkillWizard::default();
        git.dispatch(WizardEvent::SelectSource(SourceKind::Git));
        let details = SourceDetails {
            repo_url: "https://example.invalid/skills.git".to_owned(),
            ..SourceDetails::default()
        };
        git.dispatch(WizardEvent::SetDetails(Box::new(details.clone())));
        let effects = git.dispatch(WizardEvent::FetchSource);
        assert_caller_only(&effects);
        assert_eq!(
            effects,
            [WizardEffect::FetchSource {
                source: SourceKind::Git,
                details: Box::new(details),
            }]
        );
        git.dispatch(WizardEvent::SourceFetched {
            selections: vec!["repo/skill".to_owned()],
        });
        assert_eq!(git.details.fetched_repo_selections, ["repo/skill"]);
        git.dispatch(WizardEvent::ImportCompleted);
        assert_eq!(git.step, WizardStep::Source, "only Review completes");
    }

    #[test]
    fn source_details_have_independent_validation() {
        for (source, details, valid) in [
            (
                SourceKind::New,
                SourceDetails {
                    template: Some("basic".into()),
                    ..Default::default()
                },
                true,
            ),
            (
                SourceKind::Git,
                SourceDetails {
                    repo_url: "https://host/repo".into(),
                    fetched_repo_selections: vec!["s".into()],
                    ..Default::default()
                },
                true,
            ),
            (
                SourceKind::Library,
                SourceDetails {
                    library_selections: vec!["s".into()],
                    ..Default::default()
                },
                true,
            ),
            (
                SourceKind::Marketplace,
                SourceDetails {
                    marketplace_id: "owner/skill".into(),
                    ..Default::default()
                },
                true,
            ),
            (
                SourceKind::Folder,
                SourceDetails {
                    folder_path: "/folder".into(),
                    folder_selections: vec!["s".into()],
                    ..Default::default()
                },
                true,
            ),
            (
                SourceKind::Zip,
                SourceDetails {
                    zip_path: "/skill.zip".into(),
                    zip_selections: vec!["s".into()],
                    ..Default::default()
                },
                true,
            ),
        ] {
            assert_eq!(validate_details(&source, &details).is_ok(), valid);
        }
        assert_eq!(
            validate_details(&SourceKind::Git, &SourceDetails::default()),
            Err(WizardValidationError::DetailsRequired)
        );
    }
}
