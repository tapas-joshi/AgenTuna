//! Shared sandbox for the at-cli integration tests.
//!
//! Every helper here redirects AgenTuna's roots into a temporary directory, so no test
//! reads or writes the real home, store, agent folders or Keychain.
//!
//! Each integration test compiles this module separately, so a helper used by only one
//! test target is unused in the others.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};

use at_store::{AppPaths, PathEnvironment, PathOverrides};
use tempfile::TempDir;

/// A temporary AgenTuna home with every root redirected into it.
pub struct Sandbox {
    root: TempDir,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    /// Creates the temporary root.
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: TempDir::new().expect("a temp directory"),
        }
    }

    /// A path inside the sandbox; parents are not created.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The support directory: SQLite index, logs, backups and cache.
    #[must_use]
    pub fn support(&self) -> PathBuf {
        self.path("support")
    }

    /// The per-user store root.
    #[must_use]
    pub fn user_store(&self) -> PathBuf {
        self.path("user-store")
    }

    /// The machine-wide Global store root.
    #[must_use]
    pub fn global_store(&self) -> PathBuf {
        self.path("global-store")
    }

    /// Paths resolved without touching the process environment.
    #[must_use]
    pub fn env(&self) -> PathEnvironment {
        PathEnvironment::default()
            .with_home(self.path("home"))
            .with_overrides(PathOverrides {
                home: Some(self.support()),
                user_store: Some(self.user_store()),
                global_store: Some(self.global_store()),
                project_store: None,
            })
    }

    /// The resolved roots.
    #[must_use]
    pub fn paths(&self) -> AppPaths {
        AppPaths::resolve(&self.env()).expect("sandbox paths resolve")
    }

    /// Creates the directories AgenTuna owns and a healthy index inside them.
    pub fn healthy(&self) {
        let paths = self.paths();
        paths.create_dirs().expect("create the sandbox directories");
        at_db::Database::open_file(paths.db_path()).expect("create the sandbox index");
    }

    /// Environment variables that make a spawned `agentuna` use this sandbox.
    ///
    /// `HOME` is included so `~/Projects` and any other home-relative root
    /// resolve inside the sandbox instead of the developer's real home.
    #[must_use]
    pub fn vars(&self) -> [(&'static str, PathBuf); 4] {
        [
            ("AGENTUNA_HOME", self.support()),
            ("AGENTUNA_USER_STORE", self.user_store()),
            ("AGENTUNA_GLOBAL_STORE", self.global_store()),
            ("HOME", self.path("home")),
        ]
    }
}

/// Runs the real binary with the sandbox environment and no colour.
pub fn agentuna(sandbox: &Sandbox, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentuna"))
        .args(args)
        .env("NO_COLOR", "1")
        .envs(sandbox.vars())
        .output()
        .expect("spawn the agentuna binary")
}

/// Exit status of a finished run.
#[must_use]
pub fn exit_code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("the process exited with a code")
}

/// UTF-8 stdout of a finished run.
#[must_use]
pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// UTF-8 stderr of a finished run.
#[must_use]
pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
