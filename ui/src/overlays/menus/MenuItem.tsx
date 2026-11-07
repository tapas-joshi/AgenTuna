/**
 * Menu items (TU-004).
 *
 * An item is one row in a {@link Menu}. It renders its label, an optional
 * right-aligned shortcut hint and an optional leading icon. A disabled item
 * explains itself twice: hover shows the reason as a tooltip, and screen
 * readers get the reason in the row's accessible name because the menu skips
 * disabled rows during keyboard navigation.
 *
 * `mnemonic` is the Alt+letter accelerator. The matching letter is underlined
 * while the menu is open, and `aria-keyshortcuts` exposes the same binding.
 */

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as Tooltip from "@radix-ui/react-tooltip";
import { useContext, useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { OVERLAY_Z_MAX } from "../stack/OverlayStack";
import { MenuMnemonicContext } from "./menu-context";
import "./menu.css";

/** Properties of {@link MenuItem}. */
export interface MenuItemProps {
  /** Row text. Also the accessible name. Required, never empty. */
  readonly label: string;
  /** Alt+letter accelerator; the matching letter is underlined. */
  readonly mnemonic?: string;
  /** Accelerator hint shown at the end of the row, for example `⌘S`. */
  readonly shortcut?: string;
  /** Disables the row: it cannot be selected and arrow keys skip it. */
  readonly disabled?: boolean;
  /** Why the row is disabled; shown on hover and read by screen readers. */
  readonly disabledReason?: string;
  /** Visual tone. `danger` colours the row with the danger token. */
  readonly tone?: "default" | "danger";
  /** Decorative leading icon, hidden from assistive technology. */
  readonly icon?: ReactNode;
  /** Runs when the row is chosen by click, Enter or its mnemonic. */
  readonly onSelect?: () => void;
}

/** Underlines the mnemonic letter wherever it appears in the label. */
function withMnemonic(label: string, mnemonic: string | undefined): ReactNode {
  if (mnemonic === undefined || mnemonic.length === 0) {
    return label;
  }
  const index = label.toLowerCase().indexOf(mnemonic.toLowerCase());
  if (index === -1) {
    return label;
  }
  return (
    <>
      {label.slice(0, index)}
      <u>{label.charAt(index)}</u>
      {label.slice(index + 1)}
    </>
  );
}

/** One selectable row of a {@link Menu}. */
export function MenuItem({
  label,
  mnemonic,
  shortcut,
  disabled = false,
  disabledReason,
  tone = "default",
  icon,
  onSelect,
}: MenuItemProps) {
  const registry = useContext(MenuMnemonicContext);
  const selectRef = useRef(onSelect);
  useEffect(() => {
    selectRef.current = onSelect;
  });

  useEffect(() => {
    if (registry === null || mnemonic === undefined || mnemonic.length === 0) {
      return;
    }
    return registry.register(mnemonic, () => {
      if (!disabled) {
        selectRef.current?.();
      }
    });
  }, [registry, mnemonic, disabled]);

  const item = (
    <DropdownMenu.Item
      className="at-menu__item"
      data-tone={tone}
      disabled={disabled}
      aria-description={disabled ? disabledReason : undefined}
      aria-keyshortcuts={mnemonic === undefined ? undefined : `Alt+${mnemonic.toUpperCase()}`}
      onSelect={() => {
        selectRef.current?.();
      }}
    >
      {icon === undefined ? null : (
        <span className="at-menu__icon" aria-hidden="true">
          {icon}
        </span>
      )}
      <span className="at-menu__label">{withMnemonic(label, mnemonic)}</span>
      {disabled && disabledReason !== undefined ? (
        <span className="at-menu__reason-text">{`Unavailable: ${disabledReason}`}</span>
      ) : null}
      {shortcut === undefined ? null : (
        <kbd className="at-menu__shortcut" aria-hidden="true">
          {shortcut}
        </kbd>
      )}
    </DropdownMenu.Item>
  );

  if (!disabled || disabledReason === undefined) {
    return item;
  }

  // A disabled item still receives pointer events so the reason is reachable by
  // mouse; Radix Tooltip supplies the bubble, placement and `aria-describedby`.
  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>{item}</Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content
          className="at-menu__reason"
          side="right"
          sideOffset={8}
          collisionPadding={8}
          style={{ zIndex: OVERLAY_Z_MAX }}
        >
          {disabledReason}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}
