import { type RefObject, useEffect, useRef } from "react";

/**
 * Manages WAI-ARIA menu keyboard navigation (`ArrowDown` / `ArrowUp` / `Home` / `End` / `Escape`)
 * and restores focus to the trigger element when the menu closes.
 */
export function useMenuFocus(
  open: boolean,
  menuRef: RefObject<HTMLElement | null>,
  onClose?: () => void,
  triggerRef?: RefObject<HTMLElement | null>,
) {
  const savedTriggerRef = useRef<HTMLElement | null>(null);
  const wasOpenRef = useRef(false);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (open && !wasOpenRef.current) {
      savedTriggerRef.current =
        triggerRef?.current ??
        (document.activeElement instanceof HTMLElement ? document.activeElement : null);
      requestAnimationFrame(() => {
        const menu = menuRef.current;
        if (!menu) return;
        // Do not steal focus if an input inside the menu already took autoFocus (e.g. model filter).
        if (menu.contains(document.activeElement) && document.activeElement !== menu) return;
        const items = [...menu.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)')];
        const checked = items.find((el) => el.getAttribute("aria-checked") === "true");
        (checked ?? items[0])?.focus();
      });
    } else if (!open && wasOpenRef.current) {
      const target = triggerRef?.current ?? savedTriggerRef.current;
      savedTriggerRef.current = null;
      if (target && target.isConnected) {
        target.focus();
      }
    }
    wasOpenRef.current = open;
  }, [open, menuRef, triggerRef]);

  useEffect(() => {
    if (!open) return;
    const menu = menuRef.current;
    if (!menu) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && onCloseRef.current) {
        event.preventDefault();
        event.stopPropagation();
        onCloseRef.current();
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return;
      const items = [...menu.querySelectorAll<HTMLElement>('[role^="menuitem"]:not(:disabled)')];
      if (items.length === 0) return;
      event.preventDefault();
      const curIdx = items.indexOf(document.activeElement as HTMLElement);
      let nextIdx = 0;
      if (event.key === "Home") nextIdx = 0;
      else if (event.key === "End") nextIdx = items.length - 1;
      else if (event.key === "ArrowDown") nextIdx = curIdx < 0 ? 0 : (curIdx + 1) % items.length;
      else if (event.key === "ArrowUp") nextIdx = curIdx <= 0 ? items.length - 1 : curIdx - 1;
      items[nextIdx]?.focus();
    };
    menu.addEventListener("keydown", onKeyDown);
    return () => menu.removeEventListener("keydown", onKeyDown);
  }, [open, menuRef]);
}
