//! Architecture test: verifies that no code outside the helper inserts deployments directly
//! and that every production deployment write site uses the shared constructor.

use std::fs;
use std::path::{Path, PathBuf};

fn collect_production_sources(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            // Skip tests, testkit, and target directories
            if name != "tests" && name != "testkit" && name != "target" {
                collect_production_sources(&path, files);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[test]
fn no_production_code_inserts_deployments_with_raw_sql() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let crates_dir = workspace_root.join("crates");
    let mut sources = Vec::new();
    collect_production_sources(&crates_dir, &mut sources);
    assert!(!sources.is_empty(), "found workspace sources");

    let mut violations = Vec::new();

    for file in &sources {
        let rel_path = file.strip_prefix(workspace_root).unwrap_or(file);
        let path_str = rel_path.to_string_lossy();

        // at-db::deployments owns the canonical repository query, and rebuild.rs owns the batch restore
        if path_str == "crates/at-db/src/deployments.rs"
            || path_str == "crates/at-db/src/rebuild.rs"
        {
            continue;
        }

        let content = fs::read_to_string(file).expect("read source file");
        let mut in_test_module = false;

        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                in_test_module = true;
            }
            if in_test_module {
                continue;
            }

            if trimmed.contains("INSERT INTO deployments")
                || trimmed.contains("INSERT OR REPLACE INTO deployments")
            {
                violations.push(format!("{}:{}: {}", rel_path.display(), idx + 1, trimmed));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Raw SQL deployment inserts found outside at-db::deployments:\n{}",
        violations.join("\n")
    );
}

#[test]
fn all_production_deployment_writes_use_shared_applied_constructor() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let crates_dir = workspace_root.join("crates");
    let mut sources = Vec::new();
    collect_production_sources(&crates_dir, &mut sources);
    assert!(!sources.is_empty(), "found workspace sources");

    let mut violations = Vec::new();

    for file in &sources {
        let rel_path = file.strip_prefix(workspace_root).unwrap_or(file);
        let path_str = rel_path.to_string_lossy();

        // at-db::deployments defines DeploymentSpec::new
        if path_str == "crates/at-db/src/deployments.rs" {
            continue;
        }

        let content = fs::read_to_string(file).expect("read source file");
        let mut in_test_module = false;

        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                in_test_module = true;
            }
            if in_test_module {
                continue;
            }

            // Production write sites must not call DeploymentSpec::new directly without the shared constructor
            if trimmed.contains("DeploymentSpec::new(") {
                violations.push(format!("{}:{}: {}", rel_path.display(), idx + 1, trimmed));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Production code calls DeploymentSpec::new directly instead of the shared constructor:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_engine_production_code_modifies_items_or_deployments_with_raw_sql() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");

    let engine_src = workspace_root.join("crates/at-engine/src");
    let mut sources = Vec::new();
    collect_production_sources(&engine_src, &mut sources);
    assert!(!sources.is_empty(), "found at-engine production sources");

    let mut violations = Vec::new();

    let sql_patterns = [
        "INSERT INTO items",
        "INSERT OR REPLACE INTO items",
        "UPDATE items",
        "DELETE FROM items",
        "INSERT INTO deployments",
        "INSERT OR REPLACE INTO deployments",
        "UPDATE deployments",
        "DELETE FROM deployments",
    ];

    for file in &sources {
        let rel_path = file.strip_prefix(workspace_root).unwrap_or(file);
        let content = fs::read_to_string(file).expect("read source file");
        let mut in_test_module = false;

        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("mod tests {") {
                in_test_module = true;
            }
            if in_test_module {
                continue;
            }

            for pattern in &sql_patterns {
                if trimmed.contains(pattern) {
                    violations.push(format!("{}:{}: {}", rel_path.display(), idx + 1, trimmed));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Raw SQL modifications on items or deployments found in at-engine production code:\n{}",
        violations.join("\n")
    );
}
