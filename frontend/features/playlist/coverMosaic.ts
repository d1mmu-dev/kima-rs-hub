/**
 * Picks the covers for a playlist's 2x2 mosaic: the albums with the most tracks
 * first, and with two or three albums the leading ones repeat on the diagonal so
 * all four cells are filled.
 */
export function mosaicCovers(covers: Iterable<string | null | undefined>): string[] {
    const counts = new Map<string, number>();
    for (const cover of covers) {
        if (cover) counts.set(cover, (counts.get(cover) || 0) + 1);
    }
    const ranked = [...counts.entries()]
        .sort((a, b) => b[1] - a[1])
        .map(([cover]) => cover);

    if (ranked.length === 2) return [ranked[0], ranked[1], ranked[1], ranked[0]];
    if (ranked.length === 3) return [ranked[0], ranked[1], ranked[2], ranked[0]];
    return ranked.slice(0, 4);
}
