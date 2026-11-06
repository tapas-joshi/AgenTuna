//! Tauri command exposing the pure add-skill wizard state machine.

use at_engine::skill_wizard::{AddSkillWizard, WizardEffect, WizardEvent};

/// Applies one typed wizard event without performing filesystem or network work.
#[tauri::command]
pub fn skill_wizard_transition(
    wizard: Option<AddSkillWizard>,
    event: WizardEvent,
) -> (AddSkillWizard, Vec<WizardEffect>) {
    let mut wizard = wizard.unwrap_or_default();
    let effects = wizard.dispatch(event);
    (wizard, effects)
}
