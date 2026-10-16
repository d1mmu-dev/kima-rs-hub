import type { CSSProperties } from "react";

/**
 * Entrance cascade spread over a fixed window rather than a per-item delay, so
 * 12 items and 250 settle at the same moment: the window plus one 300ms rise.
 */
export const CASCADE_WINDOW_MS = 1200;

/**
 * Spread on the container; `--i` stays on each item. Without `--stagger` the
 * delay falls back to zero, so items arrive at once rather than stay hidden.
 */
export function cascadeStyle(count: number): CSSProperties {
    return {
        "--stagger": `${Math.round(CASCADE_WINDOW_MS / Math.max(count, 1))}ms`,
    } as CSSProperties;
}
