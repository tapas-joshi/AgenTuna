//! ItemId allocation order, round-trip and rejection of malformed ids.

use at_model::{ItemId, ItemIdError};

#[test]
fn ids_are_unique_and_sort_by_creation() {
    let ids: Vec<ItemId> = (0..2000).map(|_| ItemId::new()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ids must be strictly increasing"
    );
    let strings: Vec<String> = ids.iter().map(ToString::to_string).collect();
    let mut sorted = strings.clone();
    sorted.sort();
    assert_eq!(strings, sorted, "string order must match creation order");
}

#[test]
fn display_and_from_str_round_trip() {
    let id = ItemId::new();
    let text = id.to_string();
    assert!(text.starts_with("itm_"));
    assert_eq!(text.parse::<ItemId>().unwrap(), id);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<ItemId>(&json).unwrap(), id);
}

#[test]
fn rejects_malformed_ids_with_typed_errors() {
    let valid = ItemId::new().to_string();
    let upper = valid.to_uppercase().replacen("ITM_", "itm_", 1);
    assert!(matches!(
        "skill:user:tdd".parse::<ItemId>(),
        Err(ItemIdError::MissingPrefix(_))
    ));
    assert!(matches!(
        valid[4..].parse::<ItemId>(),
        Err(ItemIdError::MissingPrefix(_))
    ));
    assert!(matches!(
        "itm_not-a-uuid".parse::<ItemId>(),
        Err(ItemIdError::InvalidUuid(_))
    ));
    assert!(matches!(
        "itm_67e55044-10b1-426f-9247-bb680e5fe0c8".parse::<ItemId>(),
        Err(ItemIdError::NotVersion7(_))
    ));
    assert!(matches!(
        upper.parse::<ItemId>(),
        Err(ItemIdError::NotCanonical(_))
    ));
    assert!(serde_json::from_str::<ItemId>("\"skill:user:tdd\"").is_err());
}
