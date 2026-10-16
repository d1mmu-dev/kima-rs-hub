/**
 * Pagination primitives shared by every paged surface. `latchTotal` keeps the
 * last total seen, so a failed load never makes the pager vanish, while a real
 * zero still wins.
 */
export function parsePageParam(raw: string | null): number {
    const n = parseInt(raw ?? "1", 10);
    return Number.isFinite(n) && n >= 1 ? Math.floor(n) : 1;
}

export function latchTotal(latched: number, fresh: number | null | undefined): number {
    return fresh ?? latched;
}
