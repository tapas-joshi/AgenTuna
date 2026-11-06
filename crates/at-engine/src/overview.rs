//! Overview payload composition from the read-only database projections.

use at_model::Scope;

/// One overview card for a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCard {
    /// Scope represented by this card.
    pub scope: Scope,
    /// Item and lock counts.
    pub counts: at_db::overview::ScopeCounts,
    /// Most recent operation timestamp, or `None` when not set up.
    pub last_edited: Option<String>,
    /// Number of disabled items kept separate from effective counts.
    pub disabled_count: u64,
}

/// Complete overview query result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    /// Exactly one card per scope.
    pub scopes: Vec<ScopeCard>,
    /// Agents discovered on this machine.
    pub agents: Vec<at_db::agents::AgentSummary>,
    /// Needs-attention candidates, at most five per problem class.
    pub needs_attention: Vec<at_db::overview::AttentionCandidate>,
    /// Total number of attention candidates before per-class caps.
    pub needs_attention_total: usize,
    /// Twenty most recent operations.
    pub recent_changes: Vec<at_db::overview::RecentChangeRow>,
}

/// Builds the Overview payload using bounded, read-only database queries.
///
/// All three scope cards are returned because the Overview screen compares them together.
///
/// # Errors
/// Returns the first database error encountered.
pub fn build(
    connection: &rusqlite::Connection,
    _active_scope: Scope,
) -> Result<Overview, at_db::DbError> {
    let counts = at_db::overview::counts_for_scope(connection)?;
    let edited = at_db::overview::last_edited(connection)?;
    let shadowed = at_db::overview::shadowed_skills(connection, 500)?;
    let mut scopes = Vec::with_capacity(3);
    for scope in [Scope::Global, Scope::User, Scope::Project] {
        let mut count = counts
            .iter()
            .find(|(candidate, _)| *candidate == scope)
            .map_or_else(Default::default, |(_, count)| *count);
        if scope == Scope::Project {
            count.skills -= shadowed
                .iter()
                .filter(|row| row.scope == Scope::Project && row.winner_scope == Scope::Local)
                .count() as i64;
        }
        let last_edited = edited
            .iter()
            .find(|row| row.scope == scope)
            .and_then(|row| row.last_edited.clone());
        let disabled_count = 0;
        scopes.push(ScopeCard {
            scope,
            counts: count,
            last_edited,
            disabled_count,
        });
    }
    let mut raw_attention = at_db::overview::needs_attention(connection, 2000)?;
    let mut seen = std::collections::HashSet::new();
    raw_attention.retain(|candidate| {
        let identity = if let Some(item_id) = candidate.item_id {
            (candidate.kind, Some(item_id), None)
        } else {
            (candidate.kind, None, candidate.agent_id.clone())
        };
        seen.insert(identity)
    });
    let needs_attention_total = raw_attention.len();
    let mut class_counts = std::collections::BTreeMap::new();
    let needs_attention = raw_attention
        .into_iter()
        .filter(|candidate| {
            let count = class_counts.entry(candidate.kind).or_insert(0usize);
            if *count >= 5 {
                false
            } else {
                *count += 1;
                true
            }
        })
        .collect();
    Ok(Overview {
        scopes,
        agents: at_db::agents::list_agents_on_mac(connection)?,
        needs_attention,
        needs_attention_total,
        recent_changes: at_db::overview::recent_changes(connection, 20)?,
    })
}

/// Builds Overview with scope-indexed tombstone totals supplied by store discovery.
///
/// Tombstones live in `store.json`, not SQLite; the caller passes discovered counts.
///
/// # Errors
/// Returns the first database error encountered.
pub fn build_with_disabled_counts(
    connection: &rusqlite::Connection,
    active_scope: Scope,
    disabled_counts: &std::collections::BTreeMap<Scope, u64>,
) -> Result<Overview, at_db::DbError> {
    let mut result = build(connection, active_scope)?;
    for card in &mut result.scopes {
        card.disabled_count = disabled_counts.get(&card.scope).copied().unwrap_or(0);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_database_has_three_empty_scope_cards() {
        let db = at_db::Database::open_in_memory().expect("database");
        let result = build(db.connection(), Scope::Project).expect("overview");
        assert_eq!(result.scopes.len(), 3);
        assert!(
            result
                .scopes
                .iter()
                .all(|card| card.last_edited.is_none() && card.counts == Default::default())
        );
        assert!(result.needs_attention.is_empty());
        assert!(result.recent_changes.is_empty());
    }

    #[test]
    fn disabled_counts_are_attached_to_the_matching_scope() {
        let db = at_db::Database::open_in_memory().expect("database");
        let disabled = std::collections::BTreeMap::from([(Scope::Project, 2)]);
        let result = build_with_disabled_counts(db.connection(), Scope::Project, &disabled)
            .expect("overview");
        assert_eq!(
            result
                .scopes
                .iter()
                .find(|card| card.scope == Scope::Project)
                .expect("project card")
                .disabled_count,
            2
        );
    }
}
