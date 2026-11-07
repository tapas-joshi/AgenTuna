//! Generative tests for the normalization used by migration conflict detection.
//!
//! Normalization is what decides whether two native items are "the same": formatting must not
//! matter, and any content byte must. The cases are seeded and replay from their printed seed.

use at_fs::testkit::generative::Rng;
use at_migrate::normalize::{mcp_entry, rule_bytes};
use serde_json::Value;

const CASES: u64 = 320;

fn random_value(rng: &mut Rng, depth: usize) -> Value {
    if depth >= 2 {
        return match rng.below(3) {
            0 => Value::String(rng.unicode_word()),
            1 => Value::Bool(rng.chance(2)),
            _ => Value::Number(serde_json::Number::from(rng.below(10_000))),
        };
    }
    match rng.below(4) {
        0 => Value::String(format!("{}{}", rng.name(4), rng.unicode_word())),
        1 => Value::Array(
            (0..rng.below(4))
                .map(|_| random_value(rng, depth + 1))
                .collect(),
        ),
        2 => {
            let mut object = serde_json::Map::new();
            for _ in 0..rng.span(1, 3) {
                object.insert(rng.name(5), random_value(rng, depth + 1));
            }
            Value::Object(object)
        }
        _ => Value::Number(serde_json::Number::from(rng.below(10_000))),
    }
}

fn random_entry(rng: &mut Rng) -> Vec<(String, Value)> {
    let count = rng.span(2, 5);
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let key_len = rng.span(2, 6);
        entries.push((rng.name(key_len), random_value(rng, 0)));
    }
    entries
}

fn render(entries: &[(String, Value)], pretty: bool) -> String {
    let fields: Vec<String> = entries
        .iter()
        .map(|(key, value)| {
            format!(
                "{}:\u{20}{}",
                serde_json::to_string(key).expect("key"),
                serde_json::to_string(value).expect("value")
            )
        })
        .collect();
    if pretty {
        format!("{{\n  {}\n}}", fields.join(",\n  "))
    } else {
        format!("{{{}}}", fields.join(","))
    }
}

#[test]
fn mcp_entry_hash_ignores_json_formatting_but_not_content() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0xE17_7E57);
        let mut entries = random_entry(&mut rng);
        let first = render(&entries, false);
        // Reorder and pretty-print the same entry.
        for _ in 0..rng.span(1, 4) {
            let left = rng.below(entries.len());
            let right = rng.below(entries.len());
            entries.swap(left, right);
        }
        let second = render(&entries, true);
        let first_value: Value = serde_json::from_str(&first).expect("generated JSON");
        let second_value: Value = serde_json::from_str(&second).expect("generated JSON");
        assert_eq!(
            mcp_entry(&first_value).expect("hash"),
            mcp_entry(&second_value).expect("hash"),
            "seed={seed}: key order or whitespace changed the MCP entry hash"
        );

        // Changing one leaf is a content change.
        let mut changed = entries.clone();
        let (_key, value) = changed
            .last_mut()
            .expect("generated entries are never empty");
        *value = match &*value {
            Value::String(text) => Value::String(format!("{text}!")),
            Value::Bool(flag) => Value::Bool(!*flag),
            Value::Number(number) => Value::Number(serde_json::Number::from(
                number.as_u64().unwrap_or(0).wrapping_add(1),
            )),
            other => {
                let mut array = vec![other.clone()];
                array.push(Value::Null);
                Value::Array(array)
            }
        };
        let changed_value: Value = serde_json::from_str(&render(&changed, false)).expect("JSON");
        assert_ne!(
            mcp_entry(&first_value).expect("hash"),
            mcp_entry(&changed_value).expect("hash"),
            "seed={seed}: a content change kept the MCP entry hash"
        );
    }
}

#[test]
fn rule_hash_ignores_line_endings_and_trailing_space_but_not_content() {
    for seed in 0..CASES {
        let mut rng = Rng::new(seed ^ 0x8111_3456);
        let lines: Vec<String> = (0..rng.span(1, 6))
            .map(|_| {
                let text = if rng.chance(3) {
                    format!("# {} {}", rng.name(3), rng.unicode_word())
                } else {
                    rng.unicode_word().to_string()
                };
                if rng.chance(3) {
                    format!("{text}   \t")
                } else {
                    text
                }
            })
            .collect();
        let base = lines.join("\n");
        let lf = format!("{base}\n\n");
        let crlf = format!("{}\r\n\r\n", lines.join("\r\n"));
        assert_eq!(
            rule_bytes(lf.as_bytes()),
            rule_bytes(crlf.as_bytes()),
            "seed={seed}: line endings changed the rule hash"
        );
        let mut spaced = lines.clone();
        for line in &mut spaced {
            line.push_str("  \t");
        }
        let spaced = format!("{}\n", spaced.join("\n"));
        assert_eq!(
            rule_bytes(lf.as_bytes()),
            rule_bytes(spaced.as_bytes()),
            "seed={seed}: trailing spaces changed the rule hash"
        );

        let mut changed = lines.clone();
        let index = changed
            .iter()
            .position(|line| !line.trim().is_empty())
            .expect("at least one non-empty line");
        changed[index].push('x');
        let changed = format!("{}\n", changed.join("\n"));
        assert_ne!(
            rule_bytes(lf.as_bytes()),
            rule_bytes(changed.as_bytes()),
            "seed={seed}: a content change kept the rule hash"
        );
    }
}
