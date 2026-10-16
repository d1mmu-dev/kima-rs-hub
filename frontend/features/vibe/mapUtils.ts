/**
 * Backend mood keys, in a fixed order (the index is what the star shader sees).
 * Five moods; acoustic/electronic are a sound, not a mood, and are not shown.
 * Each value the backend sends is the track's rank on that mood within this
 * library (0..1), so moods compare fairly with each other.
 */
export const MOOD_KEYS = [
    "moodHappy",
    "moodSad",
    "moodRelaxed",
    "moodAggressive",
    "moodParty",
] as const;

// Warm, muted "matter, not neon" tones so the map reads as a night sky, with
// each mood still distinct from its neighbours.
export const MOOD_COLORS: Record<string, [number, number, number]> = {
    moodHappy:      [240, 178, 90],  // Upbeat — gold
    moodSad:        [185, 143, 224], // Melancholic — lilac
    moodRelaxed:    [91, 194, 180],  // Calm — sea green
    moodAggressive: [224, 104, 94],  // Intense — ember red
    moodParty:      [224, 106, 176], // Energetic — rose
    neutral:        [163, 163, 163], // No single mood
    unanalyzed:     [96, 96, 104],   // Not analysed yet — dimmer grey
};

const MOOD_LABEL_MAP: Record<string, string> = {
    moodHappy: "Upbeat",
    moodSad: "Melancholic",
    moodRelaxed: "Calm",
    moodAggressive: "Intense",
    moodParty: "Energetic",
    neutral: "No single mood",
    unanalyzed: "Not analysed yet",
};

/** Human name for a backend mood key ("moodHappy" → "Upbeat"). */
export function moodLabel(key: string): string {
    return MOOD_LABEL_MAP[key] ?? MOOD_LABEL_MAP.neutral;
}

/** True for one of the five moods; false for "no single mood" / "not analysed yet". */
export function isMood(key: string): boolean {
    return (MOOD_KEYS as readonly string[]).includes(key);
}

/** Index of a mood key in MOOD_KEYS, or -1 for none/neutral. */
export function moodIndex(key: string): number {
    return (MOOD_KEYS as readonly string[]).indexOf(key);
}

/** One stretch of a mood band: a palette key and its share of the analysed tracks. */
export interface MoodShare {
    key: string;
    fraction: number;
}

/** Where a mood key sits in the legend: moods in their order, "no single mood" last. */
function legendPosition(key: string): number {
    const index = moodIndex(key);
    return index >= 0 ? index : MOOD_KEYS.length;
}

/**
 * Each mood's share of the analysed tracks, biggest first, "no single mood"
 * last; equal counts keep the legend's order. Tracks not analysed yet are left
 * out of the denominator, so the band always adds up to the analysed library.
 */
export function moodShares(tracks: readonly { dominantMood: string }[]): MoodShare[] {
    const counts = new Map<string, number>();
    let analysed = 0;
    for (const track of tracks) {
        if (!track.dominantMood || track.dominantMood === "unanalyzed") continue;
        analysed += 1;
        const key = isMood(track.dominantMood) ? track.dominantMood : "neutral";
        counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    if (analysed === 0) return [];
    return [...counts.entries()]
        .sort((a, b) => {
            const posA = legendPosition(a[0]);
            const posB = legendPosition(b[0]);
            return b[1] !== a[1] ? b[1] - a[1] : posA - posB;
        })
        .map(([key, count]) => ({ key, fraction: count / analysed }));
}

/**
 * The mood a group of tracks shares: the one most of them have. No entry when
 * the winner is "no single mood" — a group that moodless is not marked.
 * Ties go to the legend's order, moods before "no single mood".
 */
export function moodByGroup(groups: readonly { id: string; dominantMood: string }[]): Map<string, string> {
    const tallies = new Map<string, Map<string, number>>();
    for (const { id, dominantMood } of groups) {
        if (!dominantMood || dominantMood === "unanalyzed") continue;
        const key = isMood(dominantMood) ? dominantMood : "neutral";
        let tally = tallies.get(id);
        if (!tally) {
            tally = new Map();
            tallies.set(id, tally);
        }
        tally.set(key, (tally.get(key) ?? 0) + 1);
    }
    const winner = new Map<string, string>();
    for (const [id, tally] of tallies) {
        let bestKey: string | null = null;
        let bestCount = -1;
        let bestPosition = Number.MAX_SAFE_INTEGER;
        for (const [key, count] of tally) {
            const position = legendPosition(key);
            if (count > bestCount || (count === bestCount && position < bestPosition)) {
                bestKey = key;
                bestCount = count;
                bestPosition = position;
            }
        }
        if (bestKey && bestKey !== "neutral") winner.set(id, bestKey);
    }
    return winner;
}
