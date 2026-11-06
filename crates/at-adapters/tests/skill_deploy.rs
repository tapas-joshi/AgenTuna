//! Skill path, local exclusion and materialization capability tests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use at_adapters::adapter::{AgentAdapterRegistry, AgentEnvironment};
use at_model::{AgentId, Scope};

fn env(home: PathBuf, project: PathBuf) -> AgentEnvironment {
    AgentEnvironment {
        home,
        project: Some(project),
        variables: BTreeMap::new(),
    }
}

#[test]
fn omp_prefers_existing_legacy_root_and_local_plan_excludes_shared_path_once() {
    let temp = tempfile::tempdir().expect("temp root");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    let legacy = home.join(".omp/agent/skills");
    std::fs::create_dir_all(&legacy).expect("legacy skills root");
    std::fs::create_dir_all(project.join(".git/info")).expect("git info");
    let adapter = AgentAdapterRegistry::register()
        .get(&AgentId::Omp)
        .expect("registry")
        .expect("omp adapter");
    assert_eq!(
        adapter
            .skill_target(Scope::User, &env(home, project.clone()), "review")
            .expect("skill target"),
        legacy.join("review")
    );

    let source = project.join(".agentuna/local/skills/review");
    let exclusion = "/.agents/skills/review\n";
    let change = adapter
        .plan_local_skill(&project, "review", &source, Some(exclusion.as_bytes()), 1)
        .expect("local deployment plan");
    assert!(matches!(
        change.ops.first(),
        Some(at_model::FileOp::Symlink { link_path, .. })
            if link_path == &PathBuf::from(".agents/skills/review")
    ));
    assert!(matches!(
        change.ops.get(1),
        Some(at_model::FileOp::Write { bytes, path, .. })
            if path == &PathBuf::from(".git/info/exclude")
                && bytes.iter().filter(|byte| **byte == b'\n').count() == 1
    ));
}

#[test]
fn first_enable_creates_only_the_selected_skills_root() {
    let temp = tempfile::tempdir().expect("temp root");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    std::fs::create_dir_all(home.join(".omp/agent")).expect("configured omp root");
    let adapter = AgentAdapterRegistry::register()
        .get(&AgentId::Omp)
        .expect("registry")
        .expect("omp adapter");
    let ops = adapter
        .ensure_dirs(
            at_model::ItemKind::Skill,
            Scope::User,
            &env(home.clone(), project),
        )
        .expect("directory plan");
    assert_eq!(ops.len(), 1);
    assert!(matches!(
        &ops[0],
        at_model::FileOp::Mkdir { path, .. }
            if path == &home.join(".omp/agent/managed-skills")
    ));
}

#[test]
fn skill_deployment_uses_shared_at_fs_mode_policy() {
    let temp = tempfile::tempdir().expect("temp root");
    let source = temp.path().join("store/review");
    let target = temp.path().join("agent/skills/review");
    std::fs::create_dir_all(&source).expect("source folder");
    std::fs::write(source.join("SKILL.md"), "skill content").expect("source skill");
    let adapter = AgentAdapterRegistry::register()
        .get(&AgentId::Codex)
        .expect("registry")
        .expect("codex adapter");
    let outcome = adapter
        .deploy_skill(
            &source,
            &target,
            at_fs::SkillStrategy {
                mode: at_model::LinkMode::Copy,
                copy_fallback: false,
            },
        )
        .expect("copy skill");
    assert_eq!(
        std::fs::read(target.join("SKILL.md")).expect("deployed content"),
        b"skill content"
    );
    assert_eq!(outcome.mode, at_model::LinkMode::Copy);
}

#[test]
fn missing_agent_is_rejected_before_any_skills_directory_plan() {
    let temp = tempfile::tempdir().expect("temp root");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    let adapter = AgentAdapterRegistry::register()
        .get(&AgentId::Omp)
        .expect("registry")
        .expect("omp adapter");
    let result = adapter.ensure_dirs(
        at_model::ItemKind::Skill,
        Scope::User,
        &env(home.clone(), project),
    );
    assert!(matches!(
        result,
        Err(at_adapters::adapter::AgentError::NotInstalled {
            agent: AgentId::Omp,
            ..
        })
    ));
    assert!(!home.exists());
}
