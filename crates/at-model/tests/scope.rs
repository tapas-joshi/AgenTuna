//! Scope precedence, parsing and wire format.

use at_model::Scope;

#[test]
fn precedence_orders_global_below_local() {
    assert!(
        Scope::Global < Scope::User
            && Scope::User < Scope::Project
            && Scope::Project < Scope::Local
    );
    let p: Vec<u8> = Scope::ALL.iter().map(|s| s.precedence()).collect();
    assert_eq!(p, [0, 1, 2, 3]);
}

#[test]
fn from_str_ignores_case_but_serde_does_not() {
    assert_eq!("LoCaL".parse::<Scope>().unwrap(), Scope::Local);
    assert_eq!(
        serde_json::from_str::<Scope>("\"global\"").unwrap(),
        Scope::Global
    );
    assert!(serde_json::from_str::<Scope>("\"Global\"").is_err());
}

#[test]
fn invalid_input_lists_all_four_values() {
    let msg = "workspace".parse::<Scope>().unwrap_err().to_string();
    for name in ["global", "user", "project", "local"] {
        assert!(msg.contains(name), "{msg}");
    }
    assert!(msg.contains("workspace"), "{msg}");
}

#[test]
fn local_is_writable() {
    assert!(Scope::Local.is_writable());
    assert_eq!(serde_json::to_string(&Scope::Local).unwrap(), "\"local\"");
}
