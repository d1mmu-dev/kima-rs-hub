"use client";

import { api } from "@/lib/api";
import { createTrackLookup } from "./trackLookup";

/**
 * Completed listens of the current track, fetched when the track changes so
 * "45th play" stays true for the whole song. Not invalidated when a play
 * records, so it never renumbers mid-song.
 */
export const useTrackPlays = createTrackLookup(
    async (trackId) => (await api.get<{ plays: number }>(`/stats/track/${encodeURIComponent(trackId)}/plays`)).plays,
);
