import { type ClassValue, clsx } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/* text-micro is a custom @theme font size tailwind-merge does not know, so it
 * would file it as a text colour and drop it when a colour follows. Registering
 * it as a font size lets size and colour coexist. */
const twMerge = extendTailwindMerge({
    extend: {
        classGroups: {
            "font-size": [{ text: ["micro"] }],
        },
    },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
