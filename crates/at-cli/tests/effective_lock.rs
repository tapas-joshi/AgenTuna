//! TA-097: effective lock and unlock CLI envelopes and scope-denial exit codes.

mod support;

use std::error::Error;

use at_cli::exit_code;
use at_db::Database;
use at_model::{ItemId, Scope};
use serde_json::Value;
use support::{Sandbox, agentuna, exit_code as process_exit, stderr};

const REASON: &str = "Managed by organization policy";

#[test]
fn effective_lock_unlock_and_lower_scope_denial_use_the_json_contract() -> Result<(), Box<dyn Error>>
{
    let sandbox = Sandbox::new();
    sandbox.healthy();
    let item_id = ItemId::new();
    let database = Database::open_file(sandbox.paths().db_path())?;
    database.connection().execute(
        "INSERT INTO items (id, kind, scope, slug, name, state, managed) \
         VALUES (?1, 'skill', 'global', 'shared-review', 'shared-review', 'present', 1)",
        [item_id.to_string()],
    )?;
    drop(database);

    let id = item_id.to_string();
    let locked = agentuna(
        &sandbox,
        &[
            "--scope",
            "global",
            "effective",
            "lock",
            &id,
            "--reason",
            REASON,
            "--json",
        ],
    );
    assert_eq!(
        process_exit(&locked),
        exit_code::SUCCESS,
        "{}",
        stderr(&locked)
    );
    assert!(stderr(&locked).is_empty());
    let locked_envelope: Value = serde_json::from_slice(&locked.stdout)?;
    assert_eq!(locked_envelope["ok"], true);
    assert_eq!(locked_envelope["command"], "effective lock");
    assert_eq!(locked_envelope["data"]["locked"], true);
    assert_eq!(locked_envelope["data"]["reason"], REASON);
    assert_eq!(locked_envelope["data"]["item_id"], id);

    let denied = agentuna(
        &sandbox,
        &["--scope", "user", "effective", "unlock", &id, "--json"],
    );
    assert_eq!(
        process_exit(&denied),
        exit_code::LOCKED,
        "{}",
        stderr(&denied)
    );
    let denied_envelope: Value = serde_json::from_slice(&denied.stdout)?;
    assert_eq!(denied_envelope["ok"], false);
    assert_eq!(denied_envelope["error"]["code"], "locked");
    assert!(
        denied_envelope["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Global scope"))
    );
    assert!(
        denied_envelope["error"]["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("--scope global"))
    );
    let database = Database::open_file(sandbox.paths().db_path())?;
    assert!(at_db::locks::find_lock(database.connection(), &item_id, Scope::Global)?.is_some());
    drop(database);

    let unlocked = agentuna(
        &sandbox,
        &["--scope", "global", "effective", "unlock", &id, "--json"],
    );
    assert_eq!(
        process_exit(&unlocked),
        exit_code::SUCCESS,
        "{}",
        stderr(&unlocked)
    );
    assert!(stderr(&unlocked).is_empty());
    let unlocked_envelope: Value = serde_json::from_slice(&unlocked.stdout)?;
    assert_eq!(unlocked_envelope["ok"], true);
    assert_eq!(unlocked_envelope["command"], "effective unlock");
    assert_eq!(unlocked_envelope["data"]["locked"], false);
    assert_eq!(unlocked_envelope["data"]["reason"], REASON);
    assert_eq!(unlocked_envelope["data"]["item_id"], id);
    let database = Database::open_file(sandbox.paths().db_path())?;
    assert!(at_db::locks::find_lock(database.connection(), &item_id, Scope::Global)?.is_none());
    Ok(())
}
