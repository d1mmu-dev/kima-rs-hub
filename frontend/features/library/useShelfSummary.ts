"use client";

/**
 * How much of the library has been heard, for the collection masthead.
 * The server has no smaller route than GET /stats, and the shelf block is
 * all-time, so this is kept for ten minutes — returning to the tab costs
 * nothing. A failure keeps the last copy.
 */

import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";

interface ShelfBlock {
    tracks: { total: number; heard: number };
    /** Tracks on the shelf that have never once been played by this user. */
    neverPlayed: number;
}

interface StatsAnswer {
    shelf: ShelfBlock;
}

/** "7% of library played · 7,107 tracks never played"; null until it loads. */
function heardLine(shelf: ShelfBlock | undefined): string | null {
    if (!shelf || shelf.tracks.total <= 0) return null;
    const pct = Math.round((shelf.tracks.heard / shelf.tracks.total) * 100);
    const never = shelf.neverPlayed;
    if (never <= 0) return `${pct}% of library played`;
    return `${pct}% of library played · ${never.toLocaleString()} ${never === 1 ? "track" : "tracks"} never played`;
}

export function useShelfSummary() {
    const { data } = useQuery({
        queryKey: ["shelf-stats"],
        queryFn: () => api.get<StatsAnswer>("/stats"),
        staleTime: 1000 * 60 * 10,
        gcTime: 1000 * 60 * 60,
        retry: 1,
    });
    return { heard: heardLine(data?.shelf) };
}
