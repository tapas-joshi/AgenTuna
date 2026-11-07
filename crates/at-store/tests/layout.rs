//! Acceptance tests for the store layout and idempotent initialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use at_model::{ItemId, Scope};
use at_store::layout::{
    MANIFEST_FILE, SCHEMA_VERSION, SLUG_MAX_CHARS, Slug, SlugError, StoreError, StoreId,
    StoreLayout, init_store, unique_slug,
};
use at_store::paths::{AppPaths, PathEnvironment, PathOverrides};
use tempfile::TempDir;

fn paths_for(home: &Path) -> AppPaths {
    AppPaths::resolve(&PathEnvironment::default().with_home(home)).expect("paths resolve")
}

fn user_layout(home: &Path) -> StoreLayout {
    StoreLayout::for_scope(&paths_for(home), Scope::User).expect("user layout")
}

fn dir_mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// Every file under `root` with its bytes and modification time, for byte-for-byte idempotence.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).expect("metadata");
            if meta.is_dir() {
                out.insert(path.clone(), (Vec::new(), meta.modified().expect("mtime")));
                walk(&path, out);
            } else {
                let bytes = fs::read(&path).expect("read");
                out.insert(path, (bytes, meta.modified().expect("mtime")));
            }
        }
    }
    let mut out = BTreeMap::new();
    if root.exists() {
        walk(root, &mut out);
    }
    out
}

#[test]
fn empty_home_creates_the_documented_layout() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    assert!(
        !layout.root().exists(),
        "layout must not create on construction"
    );

    let outcome = layout.init().expect("init on empty root");

    let expected: [PathBuf; 8] = [
        layout.skills_dir(),
        layout.rules_dir(),
        layout.backups_dir(),
        layout.state_dir(),
        layout.journals_dir(),
        layout.locks_dir(),
        layout.staging_dir(),
        layout.tmp_dir(),
    ];
    for dir in &expected {
        assert!(dir.is_dir(), "{} must exist as a directory", dir.display());
        assert_eq!(dir_mode(dir), 0o700, "{} must be 0700", dir.display());
        assert!(
            outcome.created.contains(dir),
            "{} reported created",
            dir.display()
        );
    }
    for file in [layout.mcp_file(), layout.manifest_path()] {
        assert!(file.is_file(), "{} must exist", file.display());
        assert!(
            outcome.created.contains(&file),
            "{} reported created",
            file.display()
        );
    }
    assert_eq!(
        dir_mode(layout.root()),
        0o700,
        "the store root is created 0700"
    );
    assert!(
        outcome.created.contains(&layout.root().to_path_buf()),
        "the root is created"
    );
    assert_eq!(
        outcome.created.len(),
        expected.len() + 3,
        "root + directories + mcp.json + store.json"
    );
    assert!(outcome.existing.is_empty());
    assert!(outcome.foreign.is_empty());
    assert!(outcome.created_anything());
    assert!(!outcome.has_foreign_entries());

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.manifest_path()).expect("store.json"))
            .expect("json");
    assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
    assert_eq!(manifest["store_id"], outcome.store_id.to_string());
    assert_eq!(manifest["scope"], "user");
    assert_eq!(manifest["items"], serde_json::json!([]));
    assert_eq!(manifest["disabled"], serde_json::json!([]));
    assert_eq!(outcome.schema_version, SCHEMA_VERSION);
}

#[test]
fn second_init_changes_no_bytes_and_keeps_the_store_id() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    let first = layout.init().expect("first init");
    let before = snapshot(layout.root());

    let second = layout.init().expect("second init");
    let after = snapshot(layout.root());

    assert_eq!(second.store_id, first.store_id, "store_id is stable");
    assert!(
        second.created.is_empty(),
        "nothing to create: {:?}",
        second.created
    );
    assert!(!second.existing.is_empty());
    assert_eq!(before, after, "no byte and no mtime may change");
    assert_eq!(second.foreign, first.foreign);
}

#[test]
fn project_store_creates_local_and_other_scopes_do_not() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("project root");
    let paths = AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(home.path())
            .with_project_root(project.path()),
    )
    .expect("paths");

    let project_layout = StoreLayout::for_scope(&paths, Scope::Project).expect("project layout");
    project_layout.init().expect("project init");
    let local_dir = project_layout
        .local_dir()
        .expect("project store has local/");
    assert_eq!(local_dir, project_layout.root().join("local"));
    assert!(local_dir.is_dir(), "local/ must exist in a project store");

    let user = StoreLayout::for_scope(&paths, Scope::User).expect("user layout");
    user.init().expect("user init");
    assert_eq!(user.local_dir(), None, "the user store has no local/");
    assert!(!user.root().join("local").exists());

    let global = StoreLayout::for_scope(&paths, Scope::Global).expect("global layout");
    assert_eq!(global.local_dir(), None);

    // A Local layout rooted at local/ must not nest another local/ inside it.
    let local = StoreLayout::for_scope(&paths, Scope::Local).expect("local layout");
    assert_eq!(local.root(), local_dir);
    local.init().expect("local init");
    assert_eq!(local.local_dir(), None);
    assert!(!local_dir.join("local").exists());
}

#[test]
fn foreign_content_is_reported_and_left_untouched() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    fs::create_dir_all(layout.root()).expect("root");
    let notes = layout.root().join("notes.txt");
    fs::write(&notes, b"hand written\n").expect("notes");
    let occupied = layout.root().join("skills");
    fs::write(&occupied, b"not a directory\n").expect("skills as a file");
    let stray = layout.root().join("old-stuff");
    fs::create_dir(&stray).expect("stray dir");
    fs::write(stray.join("keep.md"), b"keep\n").expect("stray file");
    // The project-store git contract writes this file; it is AgenTuna's, not foreign.
    let gitignore = layout.root().join(".gitignore");
    fs::write(&gitignore, b".state/\n").expect("gitignore");

    let outcome = layout
        .init()
        .expect("init accepts a root with foreign content");

    assert!(outcome.has_foreign_entries());
    let mut expected = vec![notes.clone(), stray.clone(), occupied.clone()];
    expected.sort();
    assert_eq!(outcome.foreign, expected);
    assert_eq!(fs::read(&notes).expect("notes survive"), b"hand written\n");
    assert!(
        !outcome.foreign.contains(&gitignore),
        "the store's own .gitignore is not foreign content"
    );
    assert_eq!(
        fs::read(&gitignore).expect("gitignore survives"),
        b".state/\n"
    );
    assert_eq!(
        fs::read(&occupied).expect("skills file survives"),
        b"not a directory\n"
    );
    assert_eq!(
        fs::read(stray.join("keep.md")).expect("stray file survives"),
        b"keep\n"
    );
    // The occupied entry was not replaced, the rest of the store still came up.
    assert!(!layout.skills_dir().is_dir());
    assert!(layout.rules_dir().is_dir());
    assert!(layout.manifest_path().is_file());
}

#[test]
fn unknown_schema_aborts_and_names_the_versions() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    fs::create_dir_all(layout.root()).expect("root");
    let manifest = format!(
        "{{\n  \"schema_version\": 99,\n  \"store_id\": \"{}\",\n  \"scope\": \"user\",\n  \"updated_at\": \"2026-01-01T00:00:00Z\"\n}}\n",
        StoreId::new()
    );
    fs::write(layout.manifest_path(), manifest.as_bytes()).expect("write manifest");

    let error = layout
        .init()
        .expect_err("a newer schema must not be touched");
    match &error {
        StoreError::UnknownSchema {
            path,
            found,
            supported,
        } => {
            assert_eq!(path, &layout.manifest_path());
            assert_eq!(*found, 99);
            assert_eq!(*supported, SCHEMA_VERSION);
        }
        other => panic!("expected UnknownSchema, got {other:?}"),
    }
    let message = error.to_string();
    assert!(message.contains("99"), "{message}");
    assert!(message.contains("update AgenTuna"), "{message}");
    assert_eq!(
        fs::read_to_string(layout.manifest_path()).expect("manifest survives"),
        manifest
    );
    assert!(!layout.mcp_file().exists(), "aborting writes nothing else");
}

#[test]
fn existing_manifest_is_never_rewritten() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    fs::create_dir_all(layout.root()).expect("root");
    let store_id = StoreId::new();
    let manifest = format!(
        "{{\n  \"schema_version\": 1,\n  \"store_id\": \"{store_id}\",\n  \"scope\": \"user\",\n  \"updated_at\": \"2026-01-01T00:00:00Z\",\n  \"items\": [\n    {{\n      \"id\": \"itm_0193f0e2-0000-7000-8000-000000000000\",\n      \"kind\": \"skill\",\n      \"slug\": \"deploy\",\n      \"path\": \"skills/deploy/SKILL.md\",\n      \"hash\": \"00\"\n    }}\n  ],\n  \"disabled\": []\n}}\n"
    );
    fs::write(layout.manifest_path(), manifest.as_bytes()).expect("write manifest");

    let outcome = layout.init().expect("init keeps an existing manifest");

    assert_eq!(outcome.store_id, store_id);
    assert!(!outcome.created.contains(&layout.manifest_path()));
    assert!(outcome.existing.contains(&layout.manifest_path()));
    assert_eq!(
        fs::read_to_string(layout.manifest_path()).expect("manifest survives"),
        manifest
    );
    assert_eq!(layout.read_header().expect("header").store_id, store_id);
}

#[test]
fn slug_rules() {
    for valid in ["deploy", "a", "build-checks", "a.b_c-2", "x9"] {
        let slug = Slug::parse(valid).unwrap_or_else(|error| panic!("{valid}: {error}"));
        assert_eq!(slug.as_str(), valid);
        assert_eq!(slug.to_string(), valid);
    }

    let long = "a".repeat(SLUG_MAX_CHARS);
    assert!(Slug::parse(&long).is_ok(), "exactly the limit is allowed");
    let too_long = "a".repeat(SLUG_MAX_CHARS + 1);
    assert!(matches!(
        Slug::parse(&too_long),
        Err(SlugError::TooLong { len, limit, .. }) if len == SLUG_MAX_CHARS + 1 && limit == SLUG_MAX_CHARS
    ));

    assert!(matches!(Slug::parse(""), Err(SlugError::Empty)));
    assert!(matches!(
        Slug::parse(".hidden"),
        Err(SlugError::LeadingInvalid { character: '.', .. })
    ));
    assert!(matches!(
        Slug::parse("-flag"),
        Err(SlugError::LeadingInvalid { character: '-', .. })
    ));
    assert!(matches!(
        Slug::parse("Deploy"),
        Err(SlugError::Uppercase { character: 'D', .. })
    ));
    assert!(matches!(
        Slug::parse("a/b"),
        Err(SlugError::Separator { character: '/', .. })
    ));
    assert!(matches!(
        Slug::parse("a\\b"),
        Err(SlugError::Separator {
            character: '\\',
            ..
        })
    ));
    assert!(matches!(
        Slug::parse("a:b"),
        Err(SlugError::Separator { character: ':', .. })
    ));
    assert!(matches!(
        Slug::parse("trail."),
        Err(SlugError::TrailingDot { .. })
    ));
    assert!(matches!(
        Slug::parse("two words"),
        Err(SlugError::InvalidCharacter { character: ' ', .. })
    ));
    // Dots are allowed inside a name; only a leading dot is refused.
    assert_eq!(
        Slug::parse("dot.dot").expect("inner dot").as_str(),
        "dot.dot"
    );

    // Serde round-trip uses the same validation.
    let slug: Slug = serde_json::from_str("\"build-checks\"").expect("slug from json");
    assert_eq!(slug.as_str(), "build-checks");
    assert_eq!(
        serde_json::to_string(&slug).expect("slug json"),
        "\"build-checks\""
    );
    assert!(serde_json::from_str::<Slug>("\"Bad\"").is_err());
}

#[test]
fn colliding_slug_gets_a_numeric_suffix_and_ids_stay_distinct() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    layout.init().expect("init");

    let first = layout.allocate_skill_slug("deploy").expect("free name");
    assert_eq!(first.as_str(), "deploy");
    fs::create_dir_all(layout.skill_dir(&first)).expect("first skill dir");

    let second = layout.allocate_skill_slug("deploy").expect("collision");
    assert_eq!(second.as_str(), "deploy-2");
    fs::create_dir_all(layout.skill_dir(&second)).expect("second skill dir");

    let third = layout
        .allocate_skill_slug("deploy")
        .expect("second collision");
    assert_eq!(third.as_str(), "deploy-3");

    assert_eq!(
        layout.skill_slugs().expect("slugs"),
        BTreeSet::from(["deploy".to_owned(), "deploy-2".to_owned()])
    );
    assert_ne!(layout.skill_dir(&first), layout.skill_dir(&second));

    let first_id = ItemId::new();
    let second_id = ItemId::new();
    assert_ne!(first_id, second_id, "identity is not derived from the slug");

    // A long base is truncated so the suffixed slug stays within the limit.
    let base = "s".repeat(SLUG_MAX_CHARS);
    let long_first = layout.allocate_skill_slug(&base).expect("long free name");
    fs::create_dir_all(layout.skill_dir(&long_first)).expect("long dir");
    let long_second = layout.allocate_skill_slug(&base).expect("long collision");
    assert_eq!(long_second.as_str().chars().count(), SLUG_MAX_CHARS);
    assert!(long_second.as_str().ends_with("-2"));
}

#[test]
fn global_root_comes_from_app_paths_and_its_overrides() {
    let home = TempDir::new().expect("temp home");
    let admin = TempDir::new().expect("admin root");

    let default_paths = paths_for(home.path());
    let default_layout =
        StoreLayout::for_scope(&default_paths, Scope::Global).expect("global layout");
    assert_eq!(default_layout.root(), default_paths.global_store());
    assert_eq!(
        default_layout.root(),
        Path::new("/Library/Application Support/AgenTuna/global")
    );

    let overridden = admin.path().join("global");
    let environment = PathEnvironment::default()
        .with_home(home.path())
        .with_overrides(PathOverrides {
            global_store: Some(overridden.clone()),
            ..PathOverrides::default()
        });
    let paths = AppPaths::resolve(&environment).expect("paths");
    assert_eq!(
        StoreLayout::for_scope(&paths, Scope::Global)
            .expect("global layout")
            .root(),
        overridden,
        "AGENTUNA_GLOBAL_STORE wins"
    );
    let outcome = init_store(Scope::Global, overridden.clone()).expect("global init");
    assert_eq!(outcome.layout.scope(), Scope::Global);
    assert!(overridden.join(MANIFEST_FILE).is_file());

    // The settings override moves the Global root too, and the layout follows it.
    let mut settings_paths = paths_for(home.path());
    assert!(settings_paths.apply_settings_global_store(Some(Path::new("admin-owned/global"))));
    let settings_layout =
        StoreLayout::for_scope(&settings_paths, Scope::Global).expect("global layout");
    assert_eq!(settings_layout.root(), settings_paths.global_store());
    assert!(settings_layout.root().ends_with("admin-owned/global"));
}

#[test]
fn global_init_without_admin_access_is_a_permission_error() {
    let sandbox = TempDir::new().expect("sandbox");
    let admin = sandbox.path().join("machine-wide");
    fs::create_dir(&admin).expect("admin dir");
    fs::set_permissions(&admin, fs::Permissions::from_mode(0o500)).expect("read-only admin dir");

    // Running as root ignores the mode; the scenario cannot be reproduced then.
    let probe = admin.join(".write-probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&admin, fs::Permissions::from_mode(0o700)).expect("restore");
        eprintln!("skipped: this process can write a 0500 directory (running as root)");
        return;
    }

    let result = init_store(Scope::Global, admin.join("global"));
    fs::set_permissions(&admin, fs::Permissions::from_mode(0o700)).expect("restore");

    let error = result.expect_err("a Global store needs write access to its admin path");
    assert!(
        matches!(error, StoreError::PermissionDenied { .. }),
        "expected PermissionDenied, got {error:?}"
    );
    assert!(error.to_string().contains("does not fall back"), "{error}");
    assert!(!admin.join("global").exists(), "nothing was created");
    assert!(
        !sandbox.path().join(".agentuna").exists(),
        "no silent fallback to the user store"
    );
}

#[test]
fn local_scope_is_a_store_at_project_local() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("project");
    let paths = AppPaths::resolve(
        &PathEnvironment::default()
            .with_home(home.path())
            .with_project_root(project.path()),
    )
    .expect("paths");
    let project_layout = StoreLayout::for_scope(&paths, Scope::Project).expect("project layout");
    project_layout.init().expect("project init");

    let local = project_layout
        .local_scope()
        .expect("local scope of a project");
    assert_eq!(local.scope(), Scope::Local);
    assert_eq!(local.root(), project_layout.local_dir().expect("local dir"));

    let outcome = local.init().expect("local init");
    assert_eq!(outcome.layout.scope(), Scope::Local);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(local.manifest_path()).expect("local manifest"))
            .expect("json");
    assert_eq!(manifest["scope"], "local");
    assert!(local.skills_dir().is_dir());
    assert!(local.mcp_file().is_file());
    assert_ne!(
        local.read_header().expect("local header").store_id,
        project_layout
            .read_header()
            .expect("project header")
            .store_id,
        "each store has its own identity"
    );
}

#[test]
fn canonical_documents_have_a_stable_shape() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    layout.init().expect("init");

    assert_eq!(
        fs::read_to_string(layout.mcp_file()).expect("mcp.json"),
        "{\n  \"schema_version\": 1,\n  \"servers\": {}\n}\n",
        "mcp.json is content only, sorted keys, 2-space indent, trailing newline"
    );

    let manifest = fs::read_to_string(layout.manifest_path()).expect("store.json");
    assert!(manifest.ends_with("}\n"), "trailing newline");
    let order = [
        "\"schema_version\"",
        "\"store_id\"",
        "\"scope\"",
        "\"updated_at\"",
        "\"items\"",
        "\"disabled\"",
    ];
    let mut previous = 0;
    for key in order {
        let at = manifest
            .find(key)
            .unwrap_or_else(|| panic!("{key} missing"));
        assert!(at >= previous, "{key} is out of order in\n{manifest}");
        previous = at;
    }

    let header = layout.read_header().expect("header");
    assert_eq!(header.schema_version, SCHEMA_VERSION);
    assert_eq!(header.scope, Scope::User);
    let expected: StoreId = manifest_id(&manifest);
    assert_eq!(header.store_id, expected);
}

fn manifest_id(manifest: &str) -> StoreId {
    let value: serde_json::Value = serde_json::from_str(manifest).expect("json");
    value["store_id"]
        .as_str()
        .expect("store_id string")
        .parse()
        .expect("store_id")
}

#[test]
fn unique_slug_reports_exhaustion() {
    let saturated: BTreeSet<String> = (0..=at_store::layout::SLUG_MAX_SUFFIX)
        .map(|number| {
            if number == 0 {
                "x".to_owned()
            } else {
                format!("x-{number}")
            }
        })
        .collect();
    assert!(matches!(
        unique_slug("x", |candidate| saturated.contains(candidate)),
        Err(SlugError::Exhausted { base }) if base == "x"
    ));
    assert_eq!(unique_slug("x", |_| false).expect("free").as_str(), "x");
}

#[test]
fn plan_init_plans_the_documented_layout_without_writing() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    let plan = layout.plan_init().expect("plan empty store");

    assert!(!layout.root().exists(), "planning must not touch the disk");
    assert!(plan.creates_root);
    let created = plan.created.iter().cloned().collect::<BTreeSet<_>>();
    let state = Path::new(".state");
    for entry in [
        PathBuf::from("skills"),
        PathBuf::from("rules"),
        PathBuf::from("backups"),
        state.join(at_store::layout::STAGING_DIR),
        state.join(at_store::layout::TMP_DIR),
        PathBuf::from("mcp.json"),
        PathBuf::from("store.json"),
    ] {
        assert!(
            created.contains(entry.as_path()),
            "{} must be planned; planned: {created:?}",
            entry.display()
        );
    }
    assert!(
        !created.contains(Path::new(".state")),
        "the apply pipeline creates .state/ itself"
    );
    assert!(
        !created.contains(state.join(at_store::layout::JOURNALS_DIR).as_path()),
        "the journaled apply writes its journal into .state/journal before the first operation, \
         so planning that directory would fail with `File exists`"
    );
    assert_eq!(plan.change_set.ops.len(), created.len());
    for op in &plan.change_set.ops {
        let destination = op.destination().expect("planned destination");
        assert!(
            destination.is_absolute() && destination.starts_with(layout.root()),
            "{} must be an absolute path below the store root",
            destination.display()
        );
    }
    let manifest = plan
        .change_set
        .ops
        .iter()
        .find_map(|op| match op {
            at_model::FileOp::Write { path, bytes, .. } if path == &layout.manifest_path() => {
                Some(bytes)
            }
            _ => None,
        })
        .expect("store.json is written");
    let document: serde_json::Value = serde_json::from_slice(manifest).expect("manifest json");
    assert_eq!(document["store_id"], plan.store_id.to_string());
    assert_eq!(document["scope"], "user");
    assert_eq!(document["schema_version"], SCHEMA_VERSION);
}

#[test]
fn plan_init_on_an_initialized_store_is_empty() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    let outcome = layout.init().expect("init");
    let plan = layout.plan_init().expect("plan initialized store");

    assert!(!plan.creates_root);
    assert!(!plan.creates_state_dir);
    assert!(plan.created.is_empty(), "planned: {:?}", plan.created);
    assert!(plan.change_set.ops.is_empty());
    assert_eq!(plan.store_id, outcome.store_id);
    assert!(plan.existing.contains(&PathBuf::from("store.json")));
    assert!(plan.existing.contains(&PathBuf::from("mcp.json")));
    assert!(plan.existing.contains(&PathBuf::from("skills")));
}

#[test]
fn plan_init_reports_foreign_entries_without_touching_them() {
    let home = TempDir::new().expect("temp home");
    let layout = user_layout(home.path());
    fs::create_dir_all(layout.root()).expect("store root");
    fs::write(layout.root().join("notes.txt"), b"my notes").expect("foreign file");

    let plan = layout.plan_init().expect("plan");
    assert!(plan.foreign.contains(&PathBuf::from("notes.txt")));
    assert!(!plan.change_set.ops.iter().any(|op| {
        op.destination()
            .is_some_and(|path| path.ends_with("notes.txt"))
    }));
    assert_eq!(
        fs::read(layout.root().join("notes.txt")).expect("foreign file"),
        b"my notes"
    );
}
