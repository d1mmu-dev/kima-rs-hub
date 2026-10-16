"use client";

/** Plays the tracks behind a figure the page states. */

import { useCallback, useState } from "react";
import { api } from "@/lib/api";
import { useAudioControls } from "@/lib/audio-controls-context";

interface PlayableTrack {
    id: string;
    title: string;
    duration: number;
    artist: { id: string; name: string };
    album: { id: string; title: string; coverArt: string | null };
}

type Slice = "top" | "never" | "core" | "key" | "decade";

export function usePlaySlice() {
    const { playTracks } = useAudioControls();
    const [busy, setBusy] = useState<string | null>(null);

    const play = useCallback(
        async (
            slice: Slice,
            opts: {
                key?: string;
                decade?: number;
                limit?: number;
                startIndex?: number;
                token?: string;
                /** The period on screen, so the button plays what the sentence names. Shelf slices ignore it. */
                period?: string;
            } = {},
        ) => {
            const token = opts.token ?? slice;
            setBusy(token);
            try {
                const params = new URLSearchParams({ of: slice });
                if (opts.key) params.set("key", opts.key);
                if (opts.decade) params.set("decade", String(opts.decade));
                if (opts.limit) params.set("limit", String(opts.limit));
                if (opts.period) params.set("period", opts.period);
                const { tracks } = await api.get<{ tracks: PlayableTrack[] }>(
                    `/stats/tracks?${params.toString()}`,
                );
                if (!tracks.length) return 0;
                // The player treats a missing cover as undefined, not null.
                playTracks(
                    tracks.map((t) => ({
                        ...t,
                        album: { ...t.album, coverArt: t.album.coverArt ?? undefined },
                    })),
                    Math.min(opts.startIndex ?? 0, tracks.length - 1),
                );
                return tracks.length;
            } catch {
                // A failed press is not worth a dialogue on a page about music.
                return 0;
            } finally {
                setBusy(null);
            }
        },
        [playTracks],
    );

    return { play, busy };
}
