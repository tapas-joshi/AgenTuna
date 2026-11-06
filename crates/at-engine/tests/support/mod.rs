//! Shared helpers for the fixture-driven engine test suites.
//!
//! Not every test binary uses every helper, so the module allows dead code: each test crate
//! compiles this file independently and would otherwise warn about the helpers it does not call.

#![allow(dead_code)]

use core::fmt;
use std::path::Path;

use serde_json::Value;
use similar::TextDiff;

/// A checked-in fixture root mapped to a printable placeholder.
///
/// Fixture output never contains a real temporary path: it contains `<home>/…`,
/// `<project>/…` or `<global>/…` instead, so a checked-in expectation is stable across runs.
pub struct PathTokens<'a> {
    /// Temporary home directory.
    pub home: &'a Path,
    /// Project root inside the temporary tree.
    pub project: &'a Path,
    /// Global store root.
    pub global: &'a Path,
}

impl PathTokens<'_> {
    /// Rewrites `path` relative to the most specific known root.
    ///
    /// Paths outside every root are printed as-is; a fixture that emits one fails the
    /// comparison loudly instead of silently storing a machine-specific path.
    #[must_use]
    pub fn tokenize(&self, path: &Path) -> String {
        let roots = [
            (self.project, "<project>"),
            (self.global, "<global>"),
            (self.home, "<home>"),
        ];
        for (root, token) in roots {
            if let Ok(relative) = path.strip_prefix(root) {
                let suffix = relative.to_string_lossy();
                return if suffix.is_empty() {
                    token.to_owned()
                } else {
                    format!("{token}/{suffix}")
                };
            }
        }
        path.to_string_lossy().into_owned()
    }
}

/// One difference between an expected and an actual JSON document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// The expected document has a value the actual document does not.
    Missing {
        /// Field path.
        path: String,
        /// Expected value, compact JSON.
        expected: String,
    },
    /// The actual document has a value the expected document does not.
    Unexpected {
        /// Field path.
        path: String,
        /// Actual value, compact JSON.
        actual: String,
    },
    /// Both documents have a value at the path, and they differ.
    Value {
        /// Field path.
        path: String,
        /// Expected value, compact JSON.
        expected: String,
        /// Actual value, compact JSON.
        actual: String,
    },
}

impl fmt::Display for Difference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path, expected } => {
                write!(formatter, "{path}: expected {expected}, actual <missing>")
            }
            Self::Unexpected { path, actual } => {
                write!(formatter, "{path}: expected <missing>, actual {actual}")
            }
            Self::Value {
                path,
                expected,
                actual,
            } => write!(formatter, "{path}: expected {expected}, actual {actual}"),
        }
    }
}

/// Every difference between `expected` and `actual`, in document order.
///
/// Object keys are visited in sorted order, array entries by index. Each difference names the
/// field path (`items[0].scope`) together with the expected and actual values, which is what a
/// failing matrix case reports.
#[must_use]
pub fn json_differences(expected: &Value, actual: &Value) -> Vec<Difference> {
    let mut differences = Vec::new();
    collect_differences(
        expected,
        actual,
        &mut String::from("<root>"),
        &mut differences,
    );
    differences
}

/// Formats every difference as one indented block.
#[must_use]
pub fn describe_differences(differences: &[Difference]) -> String {
    differences
        .iter()
        .map(|difference| format!("  - {difference}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_differences(
    expected: &Value,
    actual: &Value,
    path: &mut String,
    differences: &mut Vec<Difference>,
) {
    match (expected, actual) {
        (Value::Object(expected_map), Value::Object(actual_map)) => {
            let mut keys: Vec<&String> = expected_map.keys().chain(actual_map.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for key in keys {
                let length = path.len();
                if path.as_str() == "<root>" {
                    path.clear();
                    path.push_str(key);
                } else {
                    path.push('.');
                    path.push_str(key);
                }
                match (expected_map.get(key), actual_map.get(key)) {
                    (Some(expected_value), Some(actual_value)) => {
                        collect_differences(expected_value, actual_value, path, differences);
                    }
                    (Some(expected_value), None) => differences.push(Difference::Missing {
                        path: path.clone(),
                        expected: compact(expected_value),
                    }),
                    (None, Some(actual_value)) => differences.push(Difference::Unexpected {
                        path: path.clone(),
                        actual: compact(actual_value),
                    }),
                    (None, None) => {}
                }
                path.truncate(length);
            }
        }
        (Value::Array(expected_items), Value::Array(actual_items)) => {
            for index in 0..expected_items.len().max(actual_items.len()) {
                let length = path.len();
                path.push('[');
                path.push_str(&index.to_string());
                path.push(']');
                match (expected_items.get(index), actual_items.get(index)) {
                    (Some(expected_item), Some(actual_item)) => {
                        collect_differences(expected_item, actual_item, path, differences);
                    }
                    (Some(expected_item), None) => differences.push(Difference::Missing {
                        path: path.clone(),
                        expected: compact(expected_item),
                    }),
                    (None, Some(actual_item)) => differences.push(Difference::Unexpected {
                        path: path.clone(),
                        actual: compact(actual_item),
                    }),
                    (None, None) => {}
                }
                path.truncate(length);
            }
        }
        (expected_value, actual_value) if expected_value == actual_value => {}
        (expected_value, actual_value) => differences.push(Difference::Value {
            path: path.clone(),
            expected: compact(expected_value),
            actual: compact(actual_value),
        }),
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_owned())
}

/// Renders a unified diff between two documents of `name`.
///
/// The output is deterministic and uses relative labels (`a/name`, `b/name`), so it can be
/// checked in as a golden value.
#[must_use]
pub fn unified_diff(name: &str, before: &str, after: &str) -> String {
    let header_a = format!("a/{name}");
    let header_b = format!("b/{name}");
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&header_a, &header_b)
        .to_string()
}
