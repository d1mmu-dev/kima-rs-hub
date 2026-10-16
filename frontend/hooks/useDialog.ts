import { useEffect, useRef, type RefObject } from "react";

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

/* Modal dialog behaviour: Escape closes, Tab stays inside the dialog, the page
 * behind does not scroll, and focus returns to whatever opened it. */
export function useDialog(
    dialogRef: RefObject<HTMLElement | null>,
    isOpen: boolean,
    onClose: () => void,
    initialFocusRef?: RefObject<HTMLElement | null>,
) {
    const onCloseRef = useRef(onClose);
    useEffect(() => {
        onCloseRef.current = onClose;
    }, [onClose]);

    /* Recorded while closed, because an autoFocus field inside the dialog takes
     * focus before effects run. */
    const openerRef = useRef<HTMLElement | null>(null);
    useEffect(() => {
        if (isOpen) return;
        const track = (e: FocusEvent) => {
            const el = e.target as HTMLElement;
            if (!el.closest?.('[role="dialog"]')) openerRef.current = el;
        };
        openerRef.current = document.activeElement as HTMLElement | null;
        document.addEventListener("focusin", track);
        return () => document.removeEventListener("focusin", track);
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        const opener = openerRef.current;
        const focusables = () =>
            Array.from(dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? []).filter(
                (el) => !el.hasAttribute("disabled"),
            );
        if (!dialogRef.current?.contains(document.activeElement)) {
            (initialFocusRef?.current ?? focusables()[0] ?? dialogRef.current)?.focus();
        }

        const handleKeyDown = (e: KeyboardEvent) => {
            if (e.key === "Escape") {
                e.preventDefault();
                onCloseRef.current();
                return;
            }
            if (e.key !== "Tab") return;
            const items = focusables();
            if (items.length === 0) return;
            const first = items[0];
            const last = items[items.length - 1];
            if (e.shiftKey && document.activeElement === first) {
                e.preventDefault();
                last.focus();
            } else if (!e.shiftKey && document.activeElement === last) {
                e.preventDefault();
                first.focus();
            }
        };

        const overflow = document.body.style.overflow;
        document.body.style.overflow = "hidden";
        document.addEventListener("keydown", handleKeyDown);
        return () => {
            document.removeEventListener("keydown", handleKeyDown);
            document.body.style.overflow = overflow;
            opener?.focus();
        };
    }, [isOpen, dialogRef, initialFocusRef]);
}
