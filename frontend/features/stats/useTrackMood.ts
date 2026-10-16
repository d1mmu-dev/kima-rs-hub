"use client";

import { api } from "@/lib/api";
import { createTrackLookup } from "./trackLookup";

/**
 * The current track's dominant mood, the one that colours its star on the map.
 * A song not yet analysed is asked about again next time.
 */
export const useTrackMood = createTrackLookup(
    async (trackId) => (await api.get<{ dominantMood: string }>(`/vibe/mood/${encodeURIComponent(trackId)}`)).dominantMood,
    (mood) => mood !== "unanalyzed",
);
