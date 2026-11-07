//! TA-029: folder-defined custom agent Local deployments and the Git exclude entry.
//!
//! The declared skills subfolder is the deployment root, and `.git/info/exclude` names that
//! relative path exactly once — the same reviewed ChangeSet adds the link and the exclude.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use at_adapters::adapter::{AgentAdapter, AgentEnvironment};
use at_adapters::custom_agent::{CustomAgentAdapter, CustomAgentSpec, CustomMcpFormat};
use at_model::{ChangeSet, DeployMode, FileOp, Scope};
use tempfile::TempDir;

/// A throwaway agent root and project, resolved once so compared paths match.
struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    agent_root: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("temporary directory");
        let root = fs::canonicalize(dir.path()).expect("canonical fixture root");
        let agent_root = root.join("agent");
        let project = root.join("project");
        fs::create_dir_all(&agent_root).expect("agent root");
        fs::create_dir_all(project.join(".git/info")).expect("git fixture");
        fs::write(project.join(".git/info/exclude"), b"# git excludes\n").expect("seed exclude");
        Self {
            _dir: dir,
            root,
            agent_root,
            project,
        }
    }

    fn adapter(&self) -> CustomAgentAdapter {
        CustomAgentAdapter::new(CustomAgentSpec {
            id: String::from("windsurf"),
            display_name: String::from("Windsurf"),
            root: self.agent_root.clone(),
            skills_subfolder: PathBuf::from("custom-skills"),
            mcp_file: None,
            mcp_format: CustomMcpFormat::None,
            rule_files: Vec::new(),
            supports_symlink: false,
        })
        .expect("valid custom agent spec")
    }

    fn store_skill(&self) -> PathBuf {
        let skill = self.root.join("store/skills/alpha");
        fs::create_dir_all(&skill).expect("store skill");
        fs::write(skill.join("SKILL.md"), b"# alpha\n").expect("skill document");
        skill
    }
}

fn planned_exclude(set: &ChangeSet) -> &[u8] {
    set.ops
        .iter()
        .find_map(|op| match op {
            FileOp::Write { path, bytes, .. } if path == Path::new(".git/info/exclude") => {
                Some(bytes.as_slice())
            }
            _ => None,
        })
        .expect("the plan writes .git/info/exclude")
}

/// A recorder that keeps the apply pipeline happy without recording anything.
struct NoopRecorder;

impl at_fs::OperationRecorder for NoopRecorder {
    fn begin(&mut self, _: &ChangeSet) -> Result<String, String> {
        Ok(String::from("ta029-local-deploy"))
    }

    fn file_completed(
        &mut self,
        _: &str,
        _: &Path,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&Path>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn finish(&mut self, _: &str, _: bool, _: bool) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn local_deploy_uses_the_declared_subfolder_and_excludes_it_once() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let store_skill = fixture.store_skill();
    let env = AgentEnvironment {
        home: fixture.agent_root.clone(),
        project: Some(fixture.project.clone()),
        variables: BTreeMap::new(),
    };

    // The Local skills root is the declared subfolder inside the project, not the agent root.
    let candidates = adapter.paths(at_model::ItemKind::Skill, Scope::Local, &env);
    assert_eq!(candidates.len(), 1, "{candidates:?}");
    assert_eq!(
        candidates[0].path,
        fixture.project.join("custom-skills"),
        "Local deploys to <project>/<spec.skills_subfolder>"
    );

    let seeded = b"# git excludes\n";
    let change_set = adapter
        .plan_local_skill(
            &fixture.project,
            "alpha",
            &store_skill,
            Some(seeded),
            1_700_000_000,
        )
        .expect("reviewed local deployment plan");
    assert_eq!(change_set.scope, Some(Scope::Local));
    assert_eq!(change_set.ops.len(), 2, "{:?}", change_set.ops);
    match &change_set.ops[0] {
        FileOp::Symlink {
            link_path,
            target,
            mode,
        } => {
            assert_eq!(
                link_path,
                Path::new("custom-skills/alpha"),
                "the link lives under the declared subfolder"
            );
            assert_eq!(target, &store_skill);
            assert_eq!(*mode, DeployMode::Link);
        }
        other => panic!("the first op is the skill link: {other:?}"),
    }
    assert_eq!(
        planned_exclude(&change_set),
        b"# git excludes\n/custom-skills/alpha\n",
        "the exclude names the project-relative link path"
    );

    // Planning again over the already-written exclude adds no second entry: the file is
    // updated once, not once per attempt.
    let first_exclude = planned_exclude(&change_set).to_vec();
    let replanned = adapter
        .plan_local_skill(
            &fixture.project,
            "alpha",
            &store_skill,
            Some(&first_exclude),
            1_700_000_001,
        )
        .expect("second plan over the written exclude");
    assert_eq!(
        planned_exclude(&replanned),
        first_exclude.as_slice(),
        "a second plan produces the same exclude bytes, so the entry appears once"
    );

    // Applying the reviewed plan through the safe-write pipeline lands both operations.
    let mut recorder = NoopRecorder;
    let backups = fixture.root.join("backups");
    let mut context = at_fs::ApplyContext {
        store_root: &fixture.project,
        backup_folder: &backups,
        recorder: &mut recorder,
    };
    let roots = at_fs::ApplyRoots::new([fixture.project.clone()]).expect("authorized roots");
    let outcome =
        at_fs::apply_with_roots(&change_set, &mut context, &roots).expect("apply the plan");
    assert_eq!(outcome.completed, 2);

    let exclude = fs::read(fixture.project.join(".git/info/exclude")).expect("exclude bytes");
    assert_eq!(exclude, b"# git excludes\n/custom-skills/alpha\n");
    assert_eq!(
        exclude
            .split(|byte| *byte == b'\n')
            .filter(|line| *line == b"/custom-skills/alpha")
            .count(),
        1,
        "the entry is written exactly once"
    );
    let link = fixture.project.join("custom-skills/alpha");
    assert!(
        fs::symlink_metadata(&link)
            .expect("deployed link")
            .file_type()
            .is_symlink(),
        "the Local deployment is a link at the declared subfolder"
    );
    assert_eq!(
        fs::canonicalize(&link).expect("resolved link"),
        fs::canonicalize(&store_skill).expect("resolved store skill"),
        "the link resolves to the store skill"
    );
}
