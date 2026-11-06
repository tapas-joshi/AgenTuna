//! Local-scope store paths, built on the canonical [`StoreLayout`].
//!
//! Local is the fourth writable scope (integration decision 4). Its content lives in the
//! `local/` directory of the Project store (integration decision 3), and only
//! [`StoreLayout::for_scope`] chooses store roots. This module adds the Local-specific view of
//! that layout: the item path for each kind and an ownership check.
//!
//! Local content is machine-local and uncommitted; the project `.gitignore` ignores it
//! (TA-004). This type only computes paths and checks them; it never reads or writes.
//!
//! ```text
//! <project>/.agentuna/local/
//! ├── store.json
//! ├── skills/<slug>/SKILL.md
//! ├── mcp.json
//! └── rules/<slug>.md
//! ```

use std::path::{Component, Path, PathBuf};

use at_model::{ItemKind, Scope};

use crate::layout::{Slug, StoreError, StoreLayout};
use crate::paths::AppPaths;

/// Every path of the Local scope store of one project.
///
/// The wrapped [`StoreLayout`] has [`Scope::Local`] and its root is the project store's
/// `local/` directory. Construct one with [`LocalStorePaths::for_paths`] (production) or
/// [`LocalStorePaths::from_project`] (already-resolved layouts and tests).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalStorePaths {
    layout: StoreLayout,
}

impl LocalStorePaths {
    /// The Local store of the selected project.
    ///
    /// # Errors
    /// [`StoreError::NoActiveProject`] when no project is selected.
    pub fn for_paths(paths: &AppPaths) -> Result<Self, StoreError> {
        Ok(Self {
            layout: StoreLayout::for_scope(paths, Scope::Local)?,
        })
    }

    /// The Local store nested in `project`; `None` when `project` is not a Project layout.
    #[must_use]
    pub fn from_project(project: &StoreLayout) -> Option<Self> {
        project.local_scope().map(|layout| Self { layout })
    }

    /// The canonical layout every path is derived from.
    #[must_use]
    pub fn layout(&self) -> &StoreLayout {
        &self.layout
    }

    /// The Local store root (`<project>/.agentuna/local`).
    #[must_use]
    pub fn root(&self) -> &Path {
        self.layout.root()
    }

    /// The Local item manifest (`store.json`).
    #[must_use]
    pub fn store_file(&self) -> PathBuf {
        self.layout.manifest_path()
    }

    /// The directory holding one folder per skill (`skills/`).
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.layout.skills_dir()
    }

    /// Folder of one skill (`skills/<slug>`).
    #[must_use]
    pub fn skill_dir(&self, slug: &Slug) -> PathBuf {
        self.layout.skill_dir(slug)
    }

    /// The Local MCP content file (`mcp.json`).
    #[must_use]
    pub fn mcp_file(&self) -> PathBuf {
        self.layout.mcp_file()
    }

    /// The directory holding rule sources (`rules/`).
    #[must_use]
    pub fn rules_dir(&self) -> PathBuf {
        self.layout.rules_dir()
    }

    /// File of one rule (`rules/<slug>.md`). `slug` excludes the extension.
    #[must_use]
    pub fn rule_file(&self, slug: &Slug) -> PathBuf {
        self.rules_dir().join(format!("{}.md", slug.as_str()))
    }

    /// The store path an item of `kind` lives at.
    ///
    /// Skills get their own folder, rules a markdown file and MCP servers the shared
    /// `mcp.json` document.
    #[must_use]
    pub fn item_path(&self, kind: ItemKind, slug: &Slug) -> PathBuf {
        match kind {
            ItemKind::Skill => self.skill_dir(slug),
            ItemKind::McpServer => self.mcp_file(),
            ItemKind::Rule => self.rule_file(slug),
        }
    }

    /// Whether `candidate` resolves inside the Local store.
    ///
    /// The check is lexical (no filesystem access) and resolves `.` and `..` components,
    /// so `<root>/skills/../../other` does not count as inside. Symlinks are not followed;
    /// a caller that follows symlinks must validate the final target itself.
    #[must_use]
    pub fn contains(&self, candidate: &Path) -> bool {
        match (
            normalize_lexically(self.root()),
            normalize_lexically(candidate),
        ) {
            (Some(root), Some(candidate)) => candidate.starts_with(root),
            _ => false,
        }
    }
}

/// Resolves `.` and `..` lexically. Returns `None` when a `..` would escape the path start.
fn normalize_lexically(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn local() -> LocalStorePaths {
        let project = StoreLayout::new(Scope::Project, "/p/.agentuna");
        LocalStorePaths::from_project(&project).unwrap()
    }

    #[test]
    fn layout_mirrors_the_project_store_inside_local() {
        let paths = local();
        assert_eq!(paths.root(), Path::new("/p/.agentuna/local"));
        assert_eq!(
            paths.store_file(),
            Path::new("/p/.agentuna/local/store.json")
        );
        assert_eq!(paths.skills_dir(), Path::new("/p/.agentuna/local/skills"));
        let review = Slug::parse("review").unwrap();
        assert_eq!(
            paths.skill_dir(&review),
            Path::new("/p/.agentuna/local/skills/review")
        );
        assert_eq!(paths.mcp_file(), Path::new("/p/.agentuna/local/mcp.json"));
        assert_eq!(paths.rules_dir(), Path::new("/p/.agentuna/local/rules"));
        assert_eq!(
            paths.rule_file(&review),
            Path::new("/p/.agentuna/local/rules/review.md")
        );
        assert_eq!(paths.layout().scope(), Scope::Local);
    }

    #[test]
    fn only_project_layouts_have_a_local_store() {
        let user = StoreLayout::new(Scope::User, "/home/me/.agentuna");
        assert!(LocalStorePaths::from_project(&user).is_none());
    }

    #[test]
    fn item_paths_are_kind_specific() {
        let paths = local();
        let demo = Slug::parse("demo").unwrap();
        assert_eq!(
            paths.item_path(ItemKind::Skill, &demo),
            Path::new("/p/.agentuna/local/skills/demo")
        );
        assert_eq!(
            paths.item_path(ItemKind::McpServer, &demo),
            Path::new("/p/.agentuna/local/mcp.json")
        );
        assert_eq!(
            paths.item_path(ItemKind::Rule, &demo),
            Path::new("/p/.agentuna/local/rules/demo.md")
        );
    }

    #[test]
    fn contains_rejects_traversal_and_siblings() {
        let paths = local();
        assert!(paths.contains(Path::new("/p/.agentuna/local/skills/demo")));
        assert!(paths.contains(Path::new("/p/.agentuna/local/skills/../rules/a.md")));
        assert!(paths.contains(Path::new("/p/.agentuna/local/")));
        assert!(!paths.contains(Path::new("/p/.agentuna/local/../skills/demo")));
        assert!(!paths.contains(Path::new("/p/.agentuna/skills/demo")));
        assert!(!paths.contains(Path::new("/home/me/.agentuna/skills/demo")));
        assert!(!paths.contains(Path::new("relative/skills/demo")));
    }
}
