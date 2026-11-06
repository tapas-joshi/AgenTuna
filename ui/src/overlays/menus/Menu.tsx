/**
 * Menu surface (TU-004).
 *
 * A controlled dropdown menu rendered through Radix DropdownMenu:
 *
 * ```tsx
 * const [open, setOpen] = useState(false);
 *
 * <Menu
 *   open={open}
 *   onOpenChange={setOpen}
 *   trigger={<button onClick={() => setOpen(true)}>Actions</button>}
 * >
 *   <MenuItem label="Open" shortcut="⌘O" onSelect={openSkill} />
 *   <MenuItem label="Adopt…" disabled disabledReason="Not in Studio yet" />
 *   <MenuSeparator />
 *   <MenuItem label="Delete" tone="danger" onSelect={deleteSkill} />
 * </Menu>
 * ```
 *
 * Radix supplies the keyboard model the story requires: ArrowUp/ArrowDown move
 * through enabled items, Enter selects, Escape closes, and type-ahead jumps to
 * a label. This wrapper adds the accelerator layer: items may declare a
 * `mnemonic` (Alt+letter) and a right-aligned `shortcut` hint.
 */

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as Tooltip from "@radix-ui/react-tooltip";
import { useEffect, useMemo, useRef } from "react";
import type { KeyboardEvent, ReactElement, ReactNode } from "react";
import { cn } from "../../components/cn";
import { useOverlayLayer } from "../stack/OverlayStackProvider";
import { MenuMnemonicContext } from "./menu-context";
import type { MnemonicRegistry } from "./menu-context";
import "./menu.css";

/** Properties of {@link Menu}. */
export interface MenuProps {
  /** Whether the menu is open. */
  readonly open: boolean;
  /** Called with `false` on selection, Escape, outside click or Alt+mnemonic. */
  readonly onOpenChange?: (open: boolean) => void;
  /**
   * Element that opens the menu. It receives `aria-haspopup` and
   * `aria-expanded`, and it names the menu through `aria-labelledby`, so give
   * an icon-only trigger an `aria-label`.
   */
  readonly trigger: ReactElement;
  /** Alignment against the trigger. Defaults to `start`. */
  readonly align?: "start" | "end";
  /** Whether the menu keeps the rest of the page inert. Defaults to `true`. */
  readonly modal?: boolean;
  /** Extra classes for the menu panel. */
  readonly className?: string;
  /** {@link MenuItem}, {@link MenuSeparator} and {@link MenuLabel} children. */
  readonly children?: ReactNode;
}

/** A dropdown menu anchored to its trigger. Controlled by the caller. */
export function Menu({
  open,
  onOpenChange,
  trigger,
  align = "start",
  modal = true,
  className,
  children,
}: MenuProps) {
  const zIndex = useOverlayLayer("menu", open);

  // The registry is stable for the menu's lifetime; the close callback is read
  // through a ref so a re-rendered caller never leaves an Alt+key running stale
  // logic or closing against an old handler.
  const onOpenChangeRef = useRef(onOpenChange);
  useEffect(() => {
    onOpenChangeRef.current = onOpenChange;
  });

  const registry = useMemo<MnemonicRegistry>(() => {
    const actions = new Map<string, () => void>();
    return {
      register(letter, select) {
        const key = letter.toLowerCase();
        actions.set(key, select);
        return () => {
          if (actions.get(key) === select) {
            actions.delete(key);
          }
        };
      },
      run(letter) {
        const select = actions.get(letter.toLowerCase());
        if (select === undefined) {
          return false;
        }
        select();
        return true;
      },
    };
  }, []);

  const handleMnemonic = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (!event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    if (event.key.length !== 1) {
      return;
    }
    if (!registry.run(event.key)) {
      return;
    }
    // Consume the accelerator before Radix's type-ahead sees it, then close the
    // menu exactly as a click or Enter would.
    event.preventDefault();
    event.stopPropagation();
    onOpenChangeRef.current?.(false);
  };

  return (
    <DropdownMenu.Root open={open} onOpenChange={onOpenChange} modal={modal}>
      <DropdownMenu.Trigger asChild>{trigger}</DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={cn("at-menu", className)}
          align={align}
          sideOffset={4}
          collisionPadding={8}
          style={{ zIndex }}
          onKeyDownCapture={handleMnemonic}
        >
          <Tooltip.Provider delayDuration={400} skipDelayDuration={0}>
            <MenuMnemonicContext.Provider value={registry}>{children}</MenuMnemonicContext.Provider>
          </Tooltip.Provider>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/** A horizontal divider between groups of items. */
export function MenuSeparator() {
  return <DropdownMenu.Separator className="at-menu__separator" />;
}

/** A small uppercase heading above a group of items. */
export function MenuLabel({ children }: { readonly children?: ReactNode }) {
  return <DropdownMenu.Label className="at-menu__heading">{children}</DropdownMenu.Label>;
}
