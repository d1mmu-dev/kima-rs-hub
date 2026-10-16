import { isMood, moodLabel } from "../mapUtils";
import type { MapTrack } from "../types";

export interface MoodAnchor {
    mood: string;
    label: string;
    /** Normalised map coordinates (0..1) of the mood's densest pocket. */
    x: number;
    y: number;
    z: number;
    count: number;
}

const GRID = 6;

/**
 * One label position per mood: the densest pocket of that mood's tracks, found
 * on a coarse 3-D grid and placed at the mean of the tracks in that cell. Moods
 * held by only a sliver of the library get no label.
 */
export function computeMoodAnchors(tracks: MapTrack[], maxLabels = 7): MoodAnchor[] {
    const minCount = Math.max(3, Math.round(tracks.length * 0.02));
    const cells = new Map<string, Map<number, { n: number; x: number; y: number; z: number }>>();
    const totals = new Map<string, number>();

    for (const t of tracks) {
        const mood = t.dominantMood;
        if (!mood || !isMood(mood)) continue;
        totals.set(mood, (totals.get(mood) ?? 0) + 1);
        const z = t.z;
        const cx = Math.min(GRID - 1, Math.max(0, Math.floor(t.x * GRID)));
        const cy = Math.min(GRID - 1, Math.max(0, Math.floor(t.y * GRID)));
        const cz = Math.min(GRID - 1, Math.max(0, Math.floor(z * GRID)));
        const key = (cz * GRID + cy) * GRID + cx;
        let byCell = cells.get(mood);
        if (!byCell) cells.set(mood, (byCell = new Map()));
        const cell = byCell.get(key) ?? { n: 0, x: 0, y: 0, z: 0 };
        cell.n++;
        cell.x += t.x;
        cell.y += t.y;
        cell.z += z;
        byCell.set(key, cell);
    }

    const anchors: MoodAnchor[] = [];
    for (const [mood, byCell] of cells) {
        const total = totals.get(mood) ?? 0;
        if (total < minCount) continue;
        let best: { n: number; x: number; y: number; z: number } | null = null;
        for (const cell of byCell.values()) if (!best || cell.n > best.n) best = cell;
        if (!best) continue;
        anchors.push({ mood, label: moodLabel(mood), x: best.x / best.n, y: best.y / best.n, z: best.z / best.n, count: total });
    }
    return anchors.sort((a, b) => b.count - a.count).slice(0, maxLabels);
}
