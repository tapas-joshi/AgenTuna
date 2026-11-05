//! Locked JSON wire names and AgentId validation.

use at_model::{AgentId, AgentIdError, ItemKind, ItemState, LinkMode, Scope, SyncLabel};

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

#[test]
fn wire_names_are_locked() {
    let scopes: Vec<String> = Scope::ALL.iter().map(json).collect();
    assert_eq!(
        scopes,
        [r#""global""#, r#""user""#, r#""project""#, r#""local""#]
    );
    let kinds: Vec<String> = ItemKind::ALL.iter().map(json).collect();
    assert_eq!(kinds, [r#""skill""#, r#""mcp_server""#, r#""rule""#]);
    let states: Vec<String> = ItemState::ALL.iter().map(json).collect();
    assert_eq!(
        states,
        [
            r#""linked""#,
            r#""off""#,
            r#""native""#,
            r#""conflict""#,
            r#""drifted""#
        ]
    );
    let labels: Vec<String> = SyncLabel::ALL.iter().map(json).collect();
    assert_eq!(
        labels,
        [
            r#""in_sync""#,
            r#""project_only""#,
            r#""library_newer""#,
            r#""project_newer""#
        ]
    );
    let modes: Vec<String> = LinkMode::ALL.iter().map(json).collect();
    assert_eq!(modes, [r#""link""#, r#""copy""#]);
}

#[test]
fn agent_ids_use_snake_case_and_custom_prefix() {
    let ids = [
        AgentId::ClaudeCode,
        AgentId::Codex,
        AgentId::Omp,
        AgentId::Cursor,
        AgentId::custom("my-agent_2").unwrap(),
    ];
    let wire: Vec<String> = ids.iter().map(json).collect();
    assert_eq!(
        wire,
        [
            r#""claude_code""#,
            r#""codex""#,
            r#""omp""#,
            r#""cursor""#,
            r#""custom:my-agent_2""#
        ]
    );
    for id in &ids {
        assert_eq!(&serde_json::from_str::<AgentId>(&json(id)).unwrap(), id);
        assert_eq!(&id.to_string().parse::<AgentId>().unwrap(), id);
    }
}

#[test]
fn agent_id_rejects_ambiguous_or_malformed_input() {
    assert!(matches!(
        "gemini".parse::<AgentId>(),
        Err(AgentIdError::Unknown(_))
    ));
    assert!(matches!(
        "Codex".parse::<AgentId>(),
        Err(AgentIdError::Unknown(_))
    ));
    assert!(matches!(
        "custom:".parse::<AgentId>(),
        Err(AgentIdError::InvalidCustomName(_))
    ));
    assert!(matches!(
        "custom:Has Space".parse::<AgentId>(),
        Err(AgentIdError::InvalidCustomName(_))
    ));
    assert!(matches!(
        "custom:codex".parse::<AgentId>(),
        Err(AgentIdError::ReservedCustomName(_))
    ));
    assert!(serde_json::from_str::<AgentId>(r#""custom:codex""#).is_err());
}
