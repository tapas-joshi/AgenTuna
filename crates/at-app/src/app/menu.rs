//! Native menu actions shared by the application menu and command dispatcher.

use serde::{Deserialize, Serialize};

/// Stable identifiers for actions emitted by native menu items.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MenuAction {
    /// Open the app overview.
    NavigateOverview,
    /// Open the Skills screen.
    NavigateSkills,
    /// Open the MCP servers screen.
    NavigateMcpList,
    /// Open the effective configuration screen.
    NavigateEffective,
    /// Open the drift screen.
    NavigateDrift,
    /// Open the instructions screen.
    NavigateInstructions,
    /// Open the agents screen.
    NavigateAgents,
    /// Open Preferences.
    Preferences,
    /// Start the add-skill flow.
    AddSkill,
    /// Start the add-MCP-server flow.
    AddMcpServer,
    /// Start the add-rule-file flow.
    AddRuleFile,
    /// Start the import flow.
    Import,
    /// Show keyboard shortcuts.
    KeyboardShortcuts,
    /// Show application documentation.
    Documentation,
    /// Run diagnostics.
    RunDoctor,
}

impl MenuAction {
    /// Returns the stable action identifier consumed by the shared UI dispatcher.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::NavigateOverview => "navigate:overview",
            Self::NavigateSkills => "navigate:skills",
            Self::NavigateMcpList => "navigate:mcp-list",
            Self::NavigateEffective => "navigate:effective",
            Self::NavigateDrift => "navigate:drift",
            Self::NavigateInstructions => "navigate:instructions",
            Self::NavigateAgents => "navigate:agents",
            Self::Preferences => "preferences",
            Self::AddSkill => "add-skill",
            Self::AddMcpServer => "add-mcp-server",
            Self::AddRuleFile => "add-rule-file",
            Self::Import => "import",
            Self::KeyboardShortcuts => "keyboard-shortcuts",
            Self::Documentation => "documentation",
            Self::RunDoctor => "run-doctor",
        }
    }

    /// Resolves a native menu identifier into a typed action.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::NavigateOverview,
            Self::NavigateSkills,
            Self::NavigateMcpList,
            Self::NavigateEffective,
            Self::NavigateDrift,
            Self::NavigateInstructions,
            Self::NavigateAgents,
            Self::Preferences,
            Self::AddSkill,
            Self::AddMcpServer,
            Self::AddRuleFile,
            Self::Import,
            Self::KeyboardShortcuts,
            Self::Documentation,
            Self::RunDoctor,
        ]
        .into_iter()
        .find(|action| action.id() == id)
    }
}

/// A visible native menu entry, independent of any display server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuEntry {
    /// Native menu item label.
    pub label: &'static str,
    /// Action dispatched when selected, if this is an actionable item.
    pub action: Option<MenuAction>,
}

/// A native menu section and its entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuSection {
    /// Section label.
    pub label: &'static str,
    /// Menu entries in display order.
    pub entries: &'static [MenuEntry],
}

const APP: &[MenuEntry] = &[
    MenuEntry {
        label: "About AgenTuna",
        action: None,
    },
    MenuEntry {
        label: "Preferences…",
        action: Some(MenuAction::Preferences),
    },
    MenuEntry {
        label: "Quit AgenTuna",
        action: None,
    },
];
const EDIT: &[MenuEntry] = &[
    MenuEntry {
        label: "Undo",
        action: None,
    },
    MenuEntry {
        label: "Redo",
        action: None,
    },
    MenuEntry {
        label: "Cut",
        action: None,
    },
    MenuEntry {
        label: "Copy",
        action: None,
    },
    MenuEntry {
        label: "Paste",
        action: None,
    },
];
const WINDOW: &[MenuEntry] = &[
    MenuEntry {
        label: "Minimize",
        action: None,
    },
    MenuEntry {
        label: "Close Window",
        action: None,
    },
];
const FILE: &[MenuEntry] = &[
    MenuEntry {
        label: "Add Skill…",
        action: Some(MenuAction::AddSkill),
    },
    MenuEntry {
        label: "Add MCP Server…",
        action: Some(MenuAction::AddMcpServer),
    },
    MenuEntry {
        label: "Add Rule File…",
        action: Some(MenuAction::AddRuleFile),
    },
    MenuEntry {
        label: "Import…",
        action: Some(MenuAction::Import),
    },
];
const VIEW: &[MenuEntry] = &[
    MenuEntry {
        label: "Overview",
        action: Some(MenuAction::NavigateOverview),
    },
    MenuEntry {
        label: "Skills",
        action: Some(MenuAction::NavigateSkills),
    },
    MenuEntry {
        label: "MCP Servers",
        action: Some(MenuAction::NavigateMcpList),
    },
    MenuEntry {
        label: "Effective Config",
        action: Some(MenuAction::NavigateEffective),
    },
    MenuEntry {
        label: "Sync & Drift",
        action: Some(MenuAction::NavigateDrift),
    },
    MenuEntry {
        label: "Instructions",
        action: Some(MenuAction::NavigateInstructions),
    },
    MenuEntry {
        label: "Agents",
        action: Some(MenuAction::NavigateAgents),
    },
];
const HELP: &[MenuEntry] = &[
    MenuEntry {
        label: "Keyboard Shortcuts",
        action: Some(MenuAction::KeyboardShortcuts),
    },
    MenuEntry {
        label: "Documentation",
        action: Some(MenuAction::Documentation),
    },
    MenuEntry {
        label: "Run Doctor",
        action: Some(MenuAction::RunDoctor),
    },
];

/// Returns the menu sections and action IDs in their native display order.
#[must_use]
pub const fn menu_sections() -> [MenuSection; 6] {
    [
        MenuSection {
            label: "App",
            entries: APP,
        },
        MenuSection {
            label: "File",
            entries: FILE,
        },
        MenuSection {
            label: "Edit",
            entries: EDIT,
        },
        MenuSection {
            label: "View",
            entries: VIEW,
        },
        MenuSection {
            label: "Window",
            entries: WINDOW,
        },
        MenuSection {
            label: "Help",
            entries: HELP,
        },
    ]
}

/// Creates the native menu tree using the same stable IDs as the UI action dispatcher.
pub fn build(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};

    let about = PredefinedMenuItem::about(app, Some("About AgenTuna"), None)?;
    let preferences = MenuItem::with_id(
        app,
        MenuAction::Preferences.id(),
        "Preferences…",
        true,
        Some("CmdOrCtrl+,"),
    )?;
    let app_separator = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit AgenTuna"))?;
    let file_items = [
        MenuItem::with_id(
            app,
            MenuAction::AddSkill.id(),
            "Add Skill…",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::AddMcpServer.id(),
            "Add MCP Server…",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::AddRuleFile.id(),
            "Add Rule File…",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(app, MenuAction::Import.id(), "Import…", true, None::<&str>)?,
    ];
    let edit_items = [
        PredefinedMenuItem::undo(app, Some("Undo"))?,
        PredefinedMenuItem::redo(app, Some("Redo"))?,
        PredefinedMenuItem::separator(app)?,
        PredefinedMenuItem::cut(app, Some("Cut"))?,
        PredefinedMenuItem::copy(app, Some("Copy"))?,
        PredefinedMenuItem::paste(app, Some("Paste"))?,
    ];
    let view_items = [
        MenuItem::with_id(
            app,
            MenuAction::NavigateOverview.id(),
            "Overview",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateSkills.id(),
            "Skills",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateMcpList.id(),
            "MCP Servers",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateInstructions.id(),
            "Instructions",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateAgents.id(),
            "Agents",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateEffective.id(),
            "Effective Config",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::NavigateDrift.id(),
            "Sync & Drift",
            true,
            None::<&str>,
        )?,
    ];
    let help_items = [
        MenuItem::with_id(
            app,
            MenuAction::KeyboardShortcuts.id(),
            "Keyboard Shortcuts",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::Documentation.id(),
            "Documentation",
            true,
            None::<&str>,
        )?,
        MenuItem::with_id(
            app,
            MenuAction::RunDoctor.id(),
            "Run Doctor",
            true,
            None::<&str>,
        )?,
    ];
    let app_menu = Submenu::new(app, "AgenTuna", true)?;
    app_menu.append(&about)?;
    app_menu.append(&app_separator)?;
    app_menu.append(&preferences)?;
    app_menu.append(&quit)?;
    let file_menu = Submenu::new(app, "File", true)?;
    for item in &file_items {
        file_menu.append(item)?;
    }
    let edit_menu = Submenu::new(app, "Edit", true)?;
    for item in &edit_items {
        edit_menu.append(item)?;
    }
    let view_menu = Submenu::new(app, "View", true)?;
    for item in &view_items {
        view_menu.append(item)?;
    }
    let window_menu = Submenu::new(app, "Window", true)?;
    window_menu.append(&PredefinedMenuItem::minimize(app, Some("Minimize"))?)?;
    window_menu.append(&PredefinedMenuItem::close_window(
        app,
        Some("Close Window"),
    )?)?;
    let help_menu = Submenu::new(app, "Help", true)?;
    for item in &help_items {
        help_menu.append(item)?;
    }
    let menu = Menu::new(app)?;
    for submenu in [
        &app_menu,
        &file_menu,
        &edit_menu,
        &view_menu,
        &window_menu,
        &help_menu,
    ] {
        menu.append(submenu)?;
    }
    Ok(menu)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_actionable_menu_entry_round_trips_through_shared_dispatch_id() {
        let mut ids = HashSet::new();
        for entry in menu_sections().iter().flat_map(|section| section.entries) {
            if let Some(action) = entry.action {
                assert!(
                    ids.insert(action.id()),
                    "duplicate menu action id: {}",
                    action.id()
                );
                assert_eq!(MenuAction::from_id(action.id()), Some(action));
            }
        }
        assert!(ids.contains("navigate:overview"));
        assert!(ids.contains("navigate:skills"));
        assert!(ids.contains("add-skill"));

        assert!(ids.contains("run-doctor"));
    }
    #[test]
    fn native_menu_structure_has_expected_sections_and_builtins() {
        let sections = menu_sections();
        assert_eq!(
            sections.map(|section| section.label),
            ["App", "File", "Edit", "View", "Window", "Help"]
        );
        assert_eq!(APP[1].action, Some(MenuAction::Preferences));
        assert_eq!(FILE.len(), 4);
        assert_eq!(VIEW.len(), 7);
        assert_eq!(HELP.len(), 3);
        assert!(EDIT.iter().all(|entry| entry.action.is_none()));
    }
}
