"use client";

import { api } from "@/lib/api";
import { useSectionFlags } from "@/lib/section-flags";
import { createTrackLookup } from "./trackLookup";

/**
 * Completed listens of the current track, fetched when the track changes so
 * "45th play" stays true for the whole song. Not invalidated when a play
 * records, so it never renumbers mid-song.
 */
const useTrackPlaysLookup = createTrackLookup(
    async (trackId) => (await api.get<{ plays: number }>(`/stats/track/${encodeURIComponent(trackId)}/plays`)).plays,
);

/** kima-rs: nothing is asked while the server has no stats routes (D-170). */
export function useTrackPlays(trackId: string | null | undefined): number | null {
    const { stats } = useSectionFlags();
    return useTrackPlaysLookup(stats ? trackId : null);
}
