import { invoke } from "@tauri-apps/api/core";
import type { SkillVersionCompareView } from "../../generated/SkillVersionCompareView";
import type { SkillCompareView } from "../../generated/SkillCompareView";
import type { SkillDetailView } from "../../generated/SkillDetailView";
import type { SkillEditPreviewView } from "../../generated/SkillEditPreviewView";
import type { SkillEditResultView } from "../../generated/SkillEditResultView";

/** Loads the indexed skill detail and on-disk document. */
export function loadSkillDetail(itemId: string): Promise<SkillDetailView> {
  return invoke<SkillDetailView>("skill_detail_load", { itemId });
}

/** Compares the selected copy with another indexed scope copy. */
export function compareSkillCopies(itemId: string, otherScope: string): Promise<SkillCompareView> {
  return invoke<SkillCompareView>("skill_compare_copies", { itemId, otherScope });
}


/** Compares a stored document backup with the current SKILL.md. */
export function compareSkillVersion(itemId: string, versionId: string): Promise<SkillVersionCompareView> {
  return invoke<SkillVersionCompareView>("skill_version_compare", { itemId, versionId });
}
/** Produces an exact, non-mutating edit diff and binds it to the current package hash. */
export function previewSkillEdit(itemId: string, content: string): Promise<SkillEditPreviewView> {
  return invoke<SkillEditPreviewView>("skill_edit_preview", { itemId, content });
}

/** Applies the exact reviewed edit through the backend ChangeSet pipeline. */
export function applySkillEdit(itemId: string, content: string, expectedHash: string, previewId: string): Promise<SkillEditResultView> {
  return invoke<SkillEditResultView>("skill_edit_apply", { itemId, content, expectedHash, previewId });
}

/** Reveals a skill's indexed package in the system file manager. */
export function revealSkill(itemId: string): Promise<void> {
  return invoke<void>("skill_reveal", { itemId });
}
