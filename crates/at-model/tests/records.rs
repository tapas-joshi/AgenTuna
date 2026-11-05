//! Serde round-trips for record types.

use std::path::PathBuf;

use at_model::{
    Agent, AgentId, Deployment, ItemId, ItemKind, ItemState, LinkMode, Lock, Preset, PresetMember,
    Scope, SyncLabel,
};
use time::macros::datetime;

#[test]
fn lock_round_trips_with_rfc3339_timestamp() {
    let lock = Lock {
        scope: Scope::Global,
        item_id: ItemId::new(),
        reason: "org standard".to_owned(),
        locked_by: "admin".to_owned(),
        locked_at: datetime!(2026-11-05 10:30:00 UTC),
    };
    let json = serde_json::to_string(&lock).unwrap();
    assert!(json.contains("2026-11-05T10:30:00Z"), "{json}");
    assert_eq!(serde_json::from_str::<Lock>(&json).unwrap(), lock);
}

#[test]
fn deployment_preset_and_agent_round_trip() {
    let deployment = Deployment {
        state: ItemState::Drifted,
        mode: LinkMode::Copy,
        target_path: PathBuf::from(".claude/skills/tdd"),
        store_hash: Some("aa".to_owned()),
        target_hash: Some("bb".to_owned()),
        baseline_hash: None,
        sync_label: Some(SyncLabel::ProjectNewer),
    };
    let preset = Preset {
        id: "pre_1".to_owned(),
        name: "Backend".to_owned(),
        description: String::new(),
        scope: Scope::User,
        is_builtin: false,
        members: vec![PresetMember {
            item_id: ItemId::new(),
            kind: ItemKind::Skill,
            enabled: true,
        }],
    };
    let agent = Agent {
        id: AgentId::Omp,
        display_name: "omp".to_owned(),
        detected: true,
        version: None,
    };
    assert_eq!(
        serde_json::from_str::<Deployment>(&serde_json::to_string(&deployment).unwrap()).unwrap(),
        deployment
    );
    assert_eq!(
        serde_json::from_str::<Preset>(&serde_json::to_string(&preset).unwrap()).unwrap(),
        preset
    );
    assert_eq!(
        serde_json::from_str::<Agent>(&serde_json::to_string(&agent).unwrap()).unwrap(),
        agent
    );
}
