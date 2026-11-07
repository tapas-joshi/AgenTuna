//! TU-037: keep the shipped agent guide synchronized with the actual command tree.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use at_cli::production_registry;
use clap::Command;
use saphyr::{LoadableYamlNode, MarkedYaml, YamlData};

const GENERATED_START: &str = "<!-- BEGIN GENERATED COMMAND TREE -->";
const GENERATED_END: &str = "<!-- END GENERATED COMMAND TREE -->";

fn skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/agentuna/SKILL.md")
}

fn generated_command_reference() -> (Vec<String>, BTreeSet<String>) {
    let registry = production_registry().expect("production command registry is valid");
    let mut command = registry.command();
    command.build();
    let mut paths = Vec::new();
    let mut flags = BTreeSet::new();
    collect_tree(&mut command, &mut Vec::new(), &mut paths, &mut flags);
    (paths, flags)
}

fn collect_tree(
    command: &mut Command,
    path: &mut Vec<String>,
    paths: &mut Vec<String>,
    flags: &mut BTreeSet<String>,
) {
    for argument in command.get_arguments() {
        if let Some(long) = argument.get_long() {
            flags.insert(long.to_owned());
        }
    }
    paths.push(path.join(" "));
    for subcommand in command.get_subcommands_mut() {
        // clap adds a `help` subcommand to every group; it is not a command to document.
        if subcommand.get_name() == "help" {
            continue;
        }
        path.push(subcommand.get_name().to_owned());
        collect_tree(subcommand, path, paths, flags);
        path.pop();
    }
}

fn extract_generated_section(skill: &str) -> &str {
    let start = skill
        .find(GENERATED_START)
        .expect("skill has generated section start")
        + GENERATED_START.len();
    let end = skill
        .find(GENERATED_END)
        .expect("skill has generated section end");
    assert!(start < end, "generated section markers are ordered");
    &skill[start..end]
}

#[test]
fn frontmatter_is_valid_yaml_with_required_skill_metadata() {
    let skill = fs::read_to_string(skill_path()).expect("read the checked-in skill");
    assert!(skill.lines().count() < 400, "skill must stay concise");
    let rest = skill
        .strip_prefix("---\n")
        .expect("frontmatter opens with ---\n");
    let (frontmatter, _) = rest.split_once("\n---\n").expect("frontmatter closes");
    let documents = MarkedYaml::load_from_str(frontmatter).expect("frontmatter parses with saphyr");
    assert_eq!(documents.len(), 1, "frontmatter is one YAML document");
    let YamlData::Mapping(mapping) = &documents[0].data else {
        panic!("frontmatter must be a YAML mapping");
    };
    for field in ["name", "description"] {
        let value = mapping
            .iter()
            .find(|(key, _)| key.data.as_str() == Some(field))
            .and_then(|(_, value)| value.data.as_str());
        assert!(
            value.is_some_and(|text| !text.trim().is_empty()),
            "missing {field}"
        );
    }
    assert!(skill.contains("npx skills add tapas-joshi/AgenTuna"));
    assert!(!skill.contains("/Users/"));
    assert!(skill.contains("agentuna doctor --json"));
    assert!(skill.contains("Start with help when a command is unfamiliar"));
    assert!(skill.contains("## Global options"));
    assert!(skill.contains("## JSON output"));
    assert!(skill.contains("schema_version"));
    assert!(skill.contains("## Exit codes"));
    assert!(skill.contains("| Code | Name | Meaning |"));
    assert!(skill.contains("--dry-run"));
    assert!(skill.contains("Never edit agent-owned configuration files directly"));
    assert!(skill.contains("Never print secret values"));
    assert!(skill.contains("## Troubleshooting"));
    assert!(skill.contains("agentuna why"));
    assert!(!skill.contains("/home/"));
}

#[test]
fn generated_command_and_flag_reference_matches_production_registry() {
    let skill = fs::read_to_string(skill_path()).expect("read the checked-in skill");
    let generated = extract_generated_section(&skill);
    let (paths, flags) = generated_command_reference();
    for required in [
        "agents list",
        "agents inspect",
        "effective",
        "why",
        "overview",
        "rules list",
        "rules show",
        "rules save",
        "rules new",
    ] {
        assert!(
            paths.iter().any(|path| path == required),
            "production tree is missing required command `{required}`"
        );
    }
    for path in paths {
        let rendered = if path.is_empty() {
            "agentuna --help".to_owned()
        } else {
            format!("agentuna {path} --help")
        };
        assert!(
            generated.lines().any(|line| line.trim() == rendered),
            "generated reference omits command: {rendered}"
        );
    }
    for flag in flags {
        let rendered = format!("--{flag}");
        assert!(
            generated.contains(&rendered),
            "generated reference omits flag: {rendered}"
        );
    }
}

#[test]
fn every_agentuna_invocation_in_shell_fences_resolves_in_the_production_tree() {
    let skill = fs::read_to_string(skill_path()).expect("read the checked-in skill");
    let registry = production_registry().expect("production command registry is valid");
    let mut command = registry.command();
    command.build();
    let mut in_shell_fence = false;
    let mut checked = 0;
    for line in skill.lines() {
        if line.trim_start().starts_with("```") {
            in_shell_fence = !in_shell_fence;
            continue;
        }
        if !in_shell_fence {
            continue;
        }
        let Some(invocation) = line.trim().strip_prefix("agentuna") else {
            continue;
        };
        let invocation = invocation.split('|').next().unwrap_or(invocation);
        let mut args = invocation
            .split_whitespace()
            .map(|token| match token {
                "<PATH>" => "/tmp/agentuna-skill-project",
                "<PROJECT>" => "project-example",
                "<LEVELS>" => "4",
                "<SHELL>" => "bash",
                "<VENDOR:NAME=VALUE>" => "agentuna:marketplace=true",
                token => token,
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        args.insert(0, "agentuna".to_owned());
        args.push("--help".to_owned());
        match command.clone().try_get_matches_from(args) {
            Ok(_) => {}
            Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {}
            Err(error) => panic!("invalid CLI invocation in skill: {line}: {error}"),
        }
        checked += 1;
    }
    assert!(checked > 0, "skill must include fenced CLI invocations");
}
