/**
 * Menu coordination (TU-004).
 *
 * A menu owns one mnemonic registry. Items register their accelerator letter
 * while mounted; the menu content intercepts Alt+letter while it is open and
 * runs the first matching item. Keeping this in a small context means items do
 * not need to know the menu's open state, and a caller can render items through
 * their own components as long as they eventually render {@link MenuItem}.
 */

import { createContext } from "react";

/** Alt+letter accelerators for the items of one menu. */
export interface MnemonicRegistry {
  /**
   * Registers `letter` for `select`.
   *
   * @returns an unsubscribe function; a later registration for the same letter
   * wins, so stale items cannot steal the accelerator.
   */
  register(letter: string, select: () => void): () => void;
  /** Runs the action for `letter`; returns whether an item claimed it. */
  run(letter: string): boolean;
}

/** Null outside a menu, where mnemonics are not applicable. */
export const MenuMnemonicContext = createContext<MnemonicRegistry | null>(null);
