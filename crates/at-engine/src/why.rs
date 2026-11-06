//! Explain the scope decision for one item without touching disk.

use at_model::{AgentId, ItemId, Scope};
use std::path::PathBuf;

/// State of an item at one scope for the requested agent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StepStatus {
    /// No definition or tombstone exists at this scope.
    Absent,
    /// A definition exists and is inherited into the active scope.
    Inherited,
    /// A definition exists but is not effective in the active scope.
    Defined,
    /// A tombstone disables the item here.
    Disabled,
    /// A Global lock pins another definition.
    /// A Global lock pins a definition.
    Locked {
        /// Recorded lock reason.
        reason: String,
    },
    /// This definition loses to a closer definition.
    Shadowed,
    /// This definition is the effective winner.
    Winning,
    /// The owning document could not be read.
    /// The owning document could not be read.
    Unreadable {
        /// OS error description.
        os_error: String,
    },
}

/// Parsed-document location supplied by an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentLocation {
    /// File containing the entry.
    pub path: PathBuf,
    /// Byte range of the parsed entry, if known.
    pub byte_span: Option<std::ops::Range<usize>>,
    /// 1-based line from the adapter parser.
    pub line: Option<usize>,
    /// Agent-specific rendered destination.
    pub rendering_path: Option<PathBuf>,
}

/// One ordered scope explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    /// Scope represented.
    pub scope: Scope,
    /// State at the scope.
    pub status: StepStatus,
    /// Native document location, when available.
    pub location: Option<DocumentLocation>,
    /// Human-readable explanation.
    pub reason: String,
}

/// Final effective on/off state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// Whether the item is enabled for the selected agent.
    pub on: bool,
    /// Index of the single decisive trace step.
    pub decisive_step: Option<usize>,
}

/// Complete trace in Global, User, Project, Local order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhyTrace {
    /// Traced item.
    pub item_id: ItemId,
    /// Scope active in the caller.
    pub active_scope: Scope,
    /// Agent being explained.
    pub agent: AgentId,
    /// Ordered rows for all four scopes.
    pub steps: Vec<TraceStep>,
    /// Effective verdict and decisive row.
    pub verdict: Verdict,
}

/// Adapter- and resolver-produced facts consumed by [`trace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvidence {
    /// Whether the item exists at this scope.
    pub defined: bool,
    /// Whether a tombstone disables it at this scope.
    pub disabled: bool,
    /// Whether this definition is selected as winner.
    pub winning: bool,
    /// Whether a closer definition shadows this one.
    pub shadowed: bool,
    /// Global lock reason, if this item is locked.
    pub lock_reason: Option<String>,
    /// Adapter-read failure string, if unreadable.
    pub unreadable: Option<String>,
    /// Parsed source and rendered destination locations.
    pub location: Option<DocumentLocation>,
}

/// Builds a read-only trace from resolver and adapter facts.
///
/// The adapter is responsible for supplying parsed byte spans and line numbers; this function
/// never searches source text to infer a location.
#[must_use]
pub fn trace(
    item_id: ItemId,
    active_scope: Scope,
    agent: AgentId,
    evidence: [(Scope, TraceEvidence); 4],
) -> WhyTrace {
    let ordered = [Scope::Global, Scope::User, Scope::Project, Scope::Local];
    let mut steps = Vec::with_capacity(4);
    for scope in ordered {
        let fact = evidence.iter().find(|(s, _)| *s == scope).map(|(_, e)| e);
        let inherited_here = scope == active_scope
            && evidence.iter().any(|(source_scope, source)| {
                source.winning && source_scope.precedence() < active_scope.precedence()
            });
        let status = match fact {
            None if inherited_here => StepStatus::Inherited,
            None => StepStatus::Absent,
            Some(e) if e.unreadable.is_some() => StepStatus::Unreadable {
                os_error: e.unreadable.clone().unwrap_or_default(),
            },
            Some(e) if e.lock_reason.is_some() => StepStatus::Locked {
                reason: e.lock_reason.clone().unwrap_or_default(),
            },
            Some(e) if e.disabled => StepStatus::Disabled,
            Some(e) if e.winning => StepStatus::Winning,
            Some(e) if e.shadowed => StepStatus::Shadowed,
            Some(e) if e.defined && active_scope.precedence() > scope.precedence() => {
                StepStatus::Inherited
            }
            Some(e) if e.defined => StepStatus::Defined,
            Some(_) if inherited_here => StepStatus::Inherited,
            Some(_) => StepStatus::Absent,
        };
        let reason = match &status {
            StepStatus::Absent => "No definition at this scope".to_owned(),
            StepStatus::Inherited => "Defined here; inherited by the active scope".to_owned(),
            StepStatus::Defined => "Defined here but not selected".to_owned(),
            StepStatus::Disabled => "Disabled by a tombstone at this scope".to_owned(),
            StepStatus::Locked { reason } => format!("Pinned by Global lock: {reason}"),
            StepStatus::Shadowed => "A higher-precedence definition wins".to_owned(),
            StepStatus::Winning => "This definition is effective".to_owned(),
            StepStatus::Unreadable { os_error } => format!("Owning file is unreadable: {os_error}"),
        };
        steps.push(TraceStep {
            scope,
            status,
            location: fact.and_then(|e| e.location.clone()),
            reason,
        });
    }
    let decisive_step = steps.iter().position(|step| {
        matches!(
            step.status,
            StepStatus::Disabled | StepStatus::Locked { .. } | StepStatus::Winning
        )
    });
    let on = decisive_step
        .map(|i| matches!(steps[i].status, StepStatus::Winning))
        .unwrap_or(false);
    WhyTrace {
        item_id,
        active_scope,
        agent,
        steps,
        verdict: Verdict { on, decisive_step },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(
        scope: Scope,
        defined: bool,
        winning: bool,
        disabled: bool,
    ) -> (Scope, TraceEvidence) {
        (
            scope,
            TraceEvidence {
                defined,
                disabled,
                winning,
                shadowed: false,
                lock_reason: None,
                unreadable: None,
                location: Some(DocumentLocation {
                    path: PathBuf::from("/fixture/mcp.json"),
                    byte_span: Some(80..96),
                    line: Some(42),
                    rendering_path: Some(PathBuf::from("/fixture/agent.json")),
                }),
            },
        )
    }

    #[test]
    fn trace_orders_scopes_and_uses_adapter_line_evidence() {
        let trace = trace(
            ItemId::new(),
            Scope::Project,
            AgentId::ClaudeCode,
            [
                evidence(Scope::Global, false, false, false),
                evidence(Scope::User, true, true, false),
                evidence(Scope::Project, false, false, false),
                evidence(Scope::Local, false, false, false),
            ],
        );
        assert_eq!(
            trace.steps.iter().map(|s| s.scope).collect::<Vec<_>>(),
            vec![Scope::Global, Scope::User, Scope::Project, Scope::Local]
        );
        assert_eq!(
            trace.steps[1].location.as_ref().and_then(|l| l.line),
            Some(42)
        );
        assert_eq!(trace.steps[2].status, StepStatus::Inherited);
        assert!(trace.verdict.on);
    }

    #[test]
    fn trace_steps_snapshot_inherited_and_disabled_states() {
        let inherited = trace(
            ItemId::new(),
            Scope::Project,
            AgentId::Codex,
            [
                evidence(Scope::Global, true, true, false),
                evidence(Scope::User, false, false, false),
                evidence(Scope::Project, false, false, false),
                evidence(Scope::Local, false, false, false),
            ],
        );
        let inherited_snapshot = inherited
            .steps
            .iter()
            .map(|step| (step.scope, &step.status, step.reason.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(inherited_snapshot[0].0, Scope::Global);
        assert_eq!(inherited_snapshot[0].1, &StepStatus::Winning);
        assert_eq!(inherited_snapshot[2].1, &StepStatus::Inherited);
        assert_eq!(
            inherited_snapshot[2].2,
            "Defined here; inherited by the active scope"
        );

        let disabled = trace(
            ItemId::new(),
            Scope::Project,
            AgentId::Codex,
            [
                evidence(Scope::Global, false, false, false),
                evidence(Scope::User, true, false, false),
                evidence(Scope::Project, false, false, true),
                evidence(Scope::Local, false, false, false),
            ],
        );
        let disabled_snapshot = disabled
            .steps
            .iter()
            .map(|step| (step.scope, &step.status, step.reason.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(disabled_snapshot[2].0, Scope::Project);
        assert_eq!(disabled_snapshot[2].1, &StepStatus::Disabled);
        assert_eq!(
            disabled_snapshot[2].2,
            "Disabled by a tombstone at this scope"
        );
        assert!(!disabled.verdict.on);
    }

    #[test]
    fn project_tombstone_is_decisive_and_global_lock_has_reason() {
        let disabled = trace(
            ItemId::new(),
            Scope::Project,
            AgentId::Codex,
            [
                evidence(Scope::Global, false, false, false),
                evidence(Scope::User, true, false, false),
                evidence(Scope::Project, false, false, true),
                evidence(Scope::Local, false, false, false),
            ],
        );
        assert!(!disabled.verdict.on);
        assert_eq!(disabled.steps[2].status, StepStatus::Disabled);
        let mut global = evidence(Scope::Global, true, false, false);
        global.1.lock_reason = Some("organization policy".to_owned());
        let locked = trace(
            ItemId::new(),
            Scope::Project,
            AgentId::Codex,
            [
                global,
                evidence(Scope::User, false, false, false),
                evidence(Scope::Project, false, false, false),
                evidence(Scope::Local, false, false, false),
            ],
        );
        assert_eq!(
            locked.steps[0].status,
            StepStatus::Locked {
                reason: "organization policy".to_owned()
            }
        );
        assert!(!locked.verdict.on);
    }
}
