//! Configuration scopes and their precedence.

use crate::wire::wire_enum;

wire_enum! {
    /// Where a configuration item lives. Declaration order is precedence order.
    pub enum Scope {
        /// Machine or organisation level; may lock items.
        Global => "global",
        /// The current user, all repositories.
        User => "user",
        /// One repository, committed to Git.
        Project => "project",
        /// The current user in one repository; uncommitted.
        Local => "local",
    }
}

impl Scope {
    /// Precedence from 0 (Global, furthest) to 3 (Local, nearest). Higher wins.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Scope::Global => 0,
            Scope::User => 1,
            Scope::Project => 2,
            Scope::Local => 3,
        }
    }

    /// Whether AgenTuna can author content in this scope.
    ///
    /// All four scopes are authorable; Local is a real scope, not a presentation label.
    /// Whether the current process may actually write a given store (for example an
    /// admin-owned Global store) is a runtime permission check, not a property of the scope.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        true
    }
}
