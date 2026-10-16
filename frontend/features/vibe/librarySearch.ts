import type { MapTrack } from "./types";

export interface LibraryIndex {
    tracks: MapTrack[];
    /** Lower-cased, accent-folded "title artist" per track, built once. */
    haystacks: string[];
}

function fold(s: string): string {
    return s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
}

export function buildLibraryIndex(tracks: MapTrack[]): LibraryIndex {
    return { tracks, haystacks: tracks.map((t) => fold(`${t.title} ${t.artist}`)) };
}

export interface LibraryMatches {
    /** Every matching track id (drives the map highlight). */
    ids: Set<string>;
    /** How many tracks matched. */
    count: number;
    /** The best few, title-start matches first, for a result list. */
    top: MapTrack[];
}

const EMPTY: LibraryMatches = { ids: new Set(), count: 0, top: [] };

/**
 * Find tracks by title or artist. Every word of the query must appear; accents
 * and case are ignored. Tracks whose title or artist starts with the first word
 * rank ahead of ones that only contain it.
 */
export function searchLibrary(index: LibraryIndex, query: string, limit = 8): LibraryMatches {
    const words = fold(query).split(/\s+/).filter(Boolean);
    if (words.length === 0 || words.join("").length < 2) return EMPTY;

    const ids = new Set<string>();
    const starts: MapTrack[] = [];
    const contains: MapTrack[] = [];
    const first = words[0];
    for (let i = 0; i < index.haystacks.length; i++) {
        const hay = index.haystacks[i];
        if (!words.every((w) => hay.includes(w))) continue;
        const track = index.tracks[i];
        ids.add(track.id);
        if (hay.startsWith(first) || hay.includes(` ${first}`)) {
            if (starts.length < limit) starts.push(track);
        } else if (contains.length < limit) {
            contains.push(track);
        }
    }
    return { ids, count: ids.size, top: [...starts, ...contains].slice(0, limit) };
}
