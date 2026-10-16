"use client";

/**
 * A small note under the title in all three players: a dot in the colour of
 * the track's mood on the map, then "45th play". The count is left out on a
 * first listen, and the dot until the track has been analysed.
 */

import Link from "next/link";
import { useTrackPlays } from "./useTrackPlays";
import { ordinal } from "./format";
import { useTrackMood } from "./useTrackMood";
import { MOOD_COLORS, moodLabel } from "@/features/vibe/mapUtils";

export function PlayCountLine({
    trackId,
    className = "",
}: {
    /** Null for a book or a podcast, which are not counted this way. */
    trackId: string | null | undefined;
    /** Spacing belongs to the player this sits in. */
    className?: string;
}) {
    // Counted before the play in progress records, so the one being heard is
    // the next one up. See `useTrackPlays` for why that is the honest number.
    const heardBefore = useTrackPlays(trackId);
    const mood = useTrackMood(trackId);
    const colour = mood && mood !== "unanalyzed" ? MOOD_COLORS[mood] : null;
    const counted = heardBefore !== null && heardBefore >= 1;
    if (!colour && !counted) return null;

    const label = colour ? moodLabel(mood!).toLowerCase() : "";
    return (
        <span className={`inline-flex max-w-full min-w-0 items-center gap-1.5 text-micro leading-tight text-[var(--text-muted)] ${className}`.trim()}>
            {colour && (
                <span
                    role="img"
                    aria-label={label}
                    title={label}
                    className="size-1.5 shrink-0 rounded-full"
                    style={{ background: `rgb(${colour[0]} ${colour[1]} ${colour[2]})` }}
                />
            )}
            {counted && (
                <Link href="/stats" title="open stats" className="truncate hover:text-[var(--text-secondary)] hover:underline">
                    {ordinal(heardBefore + 1)} play
                </Link>
            )}
        </span>
    );
}
