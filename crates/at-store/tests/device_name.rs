//! The default device name comes from the host name, which can be anything a machine reports.
//! Whatever it is, the result must be accepted by the same validation the settings use, so a
//! machine with an unusual host name can still back up.

use at_store::remote_backup::{sanitize_device_name, validate_device_name};

#[test]
fn hostile_host_names_become_valid_device_names() {
    let long = "h".repeat(300);
    let multibyte = "é".repeat(100);
    let cases = [
        "runner/host.local",
        "back\\slash",
        "line\nbreak",
        "tab\there",
        "   padded   ",
        "",
        "///",
        long.as_str(),
        multibyte.as_str(),
    ];
    for raw in cases {
        let name = sanitize_device_name(raw);
        assert!(
            validate_device_name(&name).is_ok(),
            "`{raw:?}` sanitised to `{name}` which is not a valid device name"
        );
    }
}

#[test]
fn a_valid_host_name_is_kept_unchanged() {
    assert_eq!(sanitize_device_name("Mac-mini.local"), "Mac-mini.local");
}

#[test]
fn separators_are_replaced_not_dropped() {
    assert_eq!(sanitize_device_name("a/b"), "a-b");
}
