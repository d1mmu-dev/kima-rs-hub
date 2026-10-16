import { notify } from "@/lib/notify";

/**
 * Copy text, telling the user when it did not work. The Clipboard API exists only
 * on https and localhost, so a plain-http install falls back to a hidden textarea.
 */
export async function copyText(text: string): Promise<boolean> {
    try {
        if (navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            return true;
        }
        const area = document.createElement("textarea");
        area.value = text;
        area.setAttribute("readonly", "");
        area.style.position = "fixed";
        area.style.opacity = "0";
        document.body.appendChild(area);
        area.select();
        const copied = document.execCommand("copy");
        area.remove();
        if (!copied) throw new Error("copy refused");
        return true;
    } catch {
        notify.error("Couldn't copy. Select the text and copy it by hand.");
        return false;
    }
}
