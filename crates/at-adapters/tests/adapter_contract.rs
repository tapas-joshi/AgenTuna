//! Public behavior contract for the registered native adapters.

// Unix-only harness: POSIX symlinks and permission bits. The Windows twins arrive with the
// slice that enables the Windows test suite.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{
    AgentAdapterRegistry, AgentDeployment, AgentEnvironment, DetectionState,
};
use at_model::{AgentId, FileOp, ItemKind, Scope};

fn registered_ids() -> Vec<AgentId> {
    vec![
        AgentId::ClaudeCode,
        AgentId::Codex,
        AgentId::Omp,
        AgentId::Cursor,
        AgentId::Custom(String::from("generic")),
    ]
}

fn environment(root: &Path) -> AgentEnvironment {
    AgentEnvironment {
        home: root.join("home"),
        project: Some(root.join("project")),
        variables: Default::default(),
    }
}

fn seed_paths(
    adapter: &dyn at_adapters::adapter::AgentAdapter,
    env: &AgentEnvironment,
    kind: ItemKind,
    scope: Scope,
) -> Vec<PathBuf> {
    adapter
        .paths(kind, scope, env)
        .into_iter()
        .map(|candidate| {
            if kind == ItemKind::Skill {
                fs::create_dir_all(&candidate.path).expect("skill root");
                let skill = candidate.path.join("sample");
                fs::create_dir_all(&skill).expect("skill folder");
                fs::write(skill.join("SKILL.md"), b"---\nname: sample\n---\nBody\n")
                    .expect("skill content");
            } else if candidate.kind.ends_with("dir") {
                fs::create_dir_all(&candidate.path).expect("rule directory");
                if candidate.format.as_deref() == Some("mdc") {
                    fs::write(
                        candidate.path.join("sample.mdc"),
                        b"---\ndescription: \"Sample rule\"\nglobs:\n  - \"src/**\"\nalwaysApply: true\n---\n\n# Rules\n",
                    )
                    .expect("mdc rule file");
                } else {
                    fs::write(candidate.path.join("RULES.md"), b"# Rules\n")
                        .expect("rule file");
                }
            } else {
                if let Some(parent) = candidate.path.parent() {
                    fs::create_dir_all(parent).expect("rule parent");
                }
                fs::write(&candidate.path, b"# Rules\n").expect("rule file");
            }
            candidate.path
        })
        .collect()
}

#[test]
fn every_registered_adapter_obeys_native_adapter_contract() {
    let temp = tempfile::tempdir().expect("tempdir");
    let registry = AgentAdapterRegistry::register();
    for id in registered_ids() {
        let agent_root = temp.path().join(format!("{id:?}"));
        let env = environment(&agent_root);
        let adapter = registry.get(&id).expect("registry lock").expect("adapter");
        let detection = adapter.detect(&env);
        assert!(matches!(detection.state, DetectionState::NotInstalled));
        assert!(!detection.candidates.is_empty());
        let config_root = match id {
            AgentId::ClaudeCode => env.home.join(".claude"),
            AgentId::Codex => env.home.join(".codex"),
            AgentId::Omp => env.home.join(".omp/agent"),
            AgentId::Cursor => env.home.join(".cursor"),
            AgentId::Custom(_) => env.home.join(".agentuna/custom/skills"),
        };
        fs::create_dir_all(config_root).expect("configured temp agent");

        for scope in [Scope::User, Scope::Project] {
            let mkdirs = adapter
                .ensure_dirs(ItemKind::Skill, scope, &env)
                .expect("directory operations");
            assert!(
                mkdirs.iter().any(|op| matches!(op, FileOp::Mkdir { .. }))
                    || adapter
                        .paths(ItemKind::Skill, scope, &env)
                        .iter()
                        .all(|candidate| candidate.path.exists()),
                "{id} {scope:?} planned dirs: {mkdirs:?}"
            );
            let skill_paths = seed_paths(adapter.as_ref(), &env, ItemKind::Skill, scope);
            let rule_paths = seed_paths(adapter.as_ref(), &env, ItemKind::Rule, scope);
            assert!(!skill_paths.is_empty(), "{id} {scope:?} skills paths");
            assert!(!rule_paths.is_empty(), "{id} {scope:?} rules paths");

            let listed = adapter.list_native(&env).expect("native listing");
            assert!(
                listed.iter().any(|item| item.kind == ItemKind::Skill
                    && item.name == "sample"
                    && item.scope == scope),
                "{id} {scope:?} skills"
            );
            assert!(
                listed
                    .iter()
                    .any(|item| item.kind == ItemKind::Rule && item.scope == scope),
                "{id} {scope:?} rules"
            );
            let caps = adapter.capabilities(&env);
            assert!(caps.supports_rules);
            assert!(caps.mcp.is_some());
            if id == AgentId::ClaudeCode {
                assert!(caps.supports_symlink);
            }
            if matches!(id, AgentId::Custom(_)) {
                assert!(!caps.supports_symlink);
            }

            let skill_read = adapter
                .read_io(ItemKind::Skill, scope, &env)
                .expect("skill listing document");
            assert!(!skill_read.content.is_empty());
            let deployment = AgentDeployment {
                kind: ItemKind::Skill,
                scope,
                path: temp.path().join("planned-skill"),
                content: b"canonical skill bytes".to_vec(),
            };
            let rendered = adapter.render_io(&deployment).expect("skill render");
            assert_eq!(rendered, deployment.content);
            let write = adapter.write_io(&deployment).expect("skill write plan");
            assert_eq!(write.path, deployment.path);
            assert_eq!(write.content, deployment.content);

            let rule_read = adapter
                .read_io(ItemKind::Rule, scope, &env)
                .expect("rule read");
            if id == AgentId::Cursor {
                assert!(rule_read.content.starts_with(b"---\ndescription:"));
            } else {
                assert_eq!(rule_read.content, b"# Rules\n");
            }
            let rule_deployment = AgentDeployment {
                kind: ItemKind::Rule,
                scope,
                path: temp.path().join("generated-managed-rules.md"),
                content: b"# Managed rules\n".to_vec(),
            };
            let rendered = adapter.render_io(&rule_deployment).expect("rule rendering");
            assert!(rendered.starts_with(b"<!-- agentuna:begin -->\n# Managed rules\n"));
            assert!(rendered.ends_with(b"<!-- agentuna:end -->\n"));
            let rule_write = adapter
                .write_io(&rule_deployment)
                .expect("rule write payload");
            assert_eq!(rule_write.path, rule_deployment.path);
            assert_eq!(rule_write.content, rendered);
            let unmanaged_path = temp.path().join(format!("{id:?}-unmanaged.md"));
            fs::write(&unmanaged_path, b"hand-authored\n").expect("unmanaged rule fixture");
            let unmanaged_deployment = AgentDeployment {
                path: unmanaged_path,
                ..rule_deployment.clone()
            };
            assert!(matches!(
                adapter.render_io(&unmanaged_deployment),
                Err(at_adapters::adapter::AgentError::RuleRender(
                    at_fs::managed_block::ManagedBlockError::AdoptRequired { .. }
                ))
            ));

            let after = fs::read(&rule_read.path).expect("rule unchanged");
            assert_eq!(after, rule_read.content);
        }
    }

    assert_eq!(
        fs::read_dir(temp.path()).expect("temp root").count(),
        2 * registered_ids().len()
    );
}

#[cfg(unix)]
#[test]
fn native_listing_skips_symlinked_skill_folders_and_is_stable() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let env = environment(temp.path());
    let registry = AgentAdapterRegistry::register();
    let adapter = registry
        .get(&AgentId::Codex)
        .expect("registry")
        .expect("codex adapter");
    let root = adapter.paths(ItemKind::Skill, Scope::User, &env)[0]
        .path
        .clone();
    let real = root.join("real");
    let external = temp.path().join("outside");
    fs::create_dir_all(&real).expect("real skill");
    fs::create_dir_all(&external).expect("external skill");
    fs::write(real.join("SKILL.md"), b"# real\n").expect("real manifest");
    fs::write(external.join("SKILL.md"), b"# outside\n").expect("external manifest");
    symlink(&external, root.join("linked")).expect("skill symlink");

    let first = adapter.list_native(&env).expect("first listing");
    let second = adapter.list_native(&env).expect("second listing");
    assert_eq!(first, second);
    assert!(
        first
            .iter()
            .any(|item| item.kind == ItemKind::Skill && item.name == "real")
    );
    assert!(
        !first
            .iter()
            .any(|item| item.kind == ItemKind::Skill && item.name == "linked")
    );
    assert_eq!(
        fs::read_dir(&root).expect("skill root").count(),
        2,
        "listing is read-only"
    );
}

#[test]
fn project_shared_roots_are_grouped_by_real_destination() {
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("project");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&project).expect("project");
    let env = AgentEnvironment {
        home,
        project: Some(project),
        variables: Default::default(),
    };
    let registry = AgentAdapterRegistry::register();
    let roots = registry
        .shared_roots(Scope::Project, &env)
        .expect("shared roots");
    let project_root = fs::canonicalize(env.project.as_ref().expect("project"))
        .expect("canonical project")
        .join(".agents/skills");
    let user_root = fs::canonicalize(&env.home)
        .expect("canonical home")
        .join(".agents/skills");
    let normalize = |path: PathBuf| {
        #[cfg(target_os = "macos")]
        {
            PathBuf::from(path.to_string_lossy().to_lowercase())
        }
        #[cfg(not(target_os = "macos"))]
        {
            path
        }
    };
    assert_eq!(
        roots.get(&normalize(project_root)),
        Some(&[AgentId::Codex, AgentId::Omp].into_iter().collect())
    );
    assert_eq!(
        roots.get(&normalize(user_root)),
        Some(&[AgentId::Codex].into_iter().collect())
    );
    let project_agents_root = normalize(
        fs::canonicalize(env.project.as_ref().expect("project"))
            .expect("canonical project")
            .join(".agents/skills"),
    );
    let user_agents_root = normalize(
        fs::canonicalize(&env.home)
            .expect("canonical home")
            .join(".agents/skills"),
    );
    assert!(
        roots
            .iter()
            .filter(|(path, _)| *path != &project_agents_root && *path != &user_agents_root)
            .all(|(_, agents)| agents.len() == 1),
        "{roots:?}"
    );
}

#[test]
fn unsupported_kind_returns_unsupported_error_without_panic() {
    let temp = tempfile::tempdir().expect("tempdir");
    let env = environment(temp.path());
    let registry = AgentAdapterRegistry::register();

    // Codex adapter does not support Claude project settings
    let codex = registry
        .get(&AgentId::Codex)
        .expect("registry")
        .expect("codex");
    let result = codex.plan_settings_server_enabled(&env, "{}", "some_server", true, 0);
    assert!(
        matches!(
            &result,
            Err(at_adapters::adapter::AgentError::Unsupported { agent, kind })
                if *agent == AgentId::Codex && kind.contains("Claude project settings")
        ),
        "expected Unsupported error, got: {result:?}"
    );

    // Omp adapter does not support Claude project settings
    let omp = registry.get(&AgentId::Omp).expect("registry").expect("omp");
    let result = omp.plan_settings_server_enabled(&env, "{}", "some_server", true, 0);
    assert!(
        matches!(
            &result,
            Err(at_adapters::adapter::AgentError::Unsupported { agent, kind })
                if *agent == AgentId::Omp && kind.contains("Claude project settings")
        ),
        "expected Unsupported error, got: {result:?}"
    );
}
