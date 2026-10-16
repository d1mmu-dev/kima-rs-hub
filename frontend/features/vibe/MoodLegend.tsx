"use client";

import { useMemo } from "react";
import { MOOD_COLORS, isMood, moodLabel } from "./mapUtils";
import type { MapTrack } from "./types";

interface MoodLegendProps {
    tracks: MapTrack[];
    /** Every track on the map, shown after the moods. */
    trackCount: number;
    activeMood: string | null;
    /** One scrolling row instead of wrapping (phones). */
    scroll?: boolean;
    onPreview: (mood: string | null) => void;
    onToggle: (mood: string) => void;
}

/**
 * The library's moods, counted, largest first, then "No single mood" and "Not
 * analysed yet". Hover lights a mood's tracks; a click keeps it lit.
 */
export function MoodLegend({ tracks, trackCount, activeMood, scroll = false, onPreview, onToggle }: MoodLegendProps) {
    const moods = useMemo(() => {
        const counts = new Map<string, number>();
        for (const t of tracks) counts.set(t.dominantMood, (counts.get(t.dominantMood) ?? 0) + 1);
        return [...counts.entries()].sort(
            (a, b) => Number(isMood(b[0])) - Number(isMood(a[0])) || b[1] - a[1],
        );
    }, [tracks]);

    const total = <span className="shrink-0 text-[11px] tabular-nums text-white/60">{trackCount.toLocaleString()} tracks</span>;

    return (
        <>
            <div
                className={scroll
                    ? "-mx-3 flex items-center gap-1.5 overflow-x-auto px-3 [scrollbar-width:none] [mask-image:linear-gradient(to_right,transparent,black_0.75rem,black_calc(100%-1.5rem),transparent)]"
                    : "flex flex-wrap gap-1.5"}
                onMouseLeave={() => onPreview(null)}
                role="group"
                aria-label="Moods in your library"
            >
                {moods.map(([mood, count]) => {
                    const [r, g, b] = MOOD_COLORS[mood] ?? MOOD_COLORS.neutral;
                    const on = activeMood === mood;
                    return (
                        <button
                            key={mood}
                            type="button"
                            aria-pressed={on}
                            onClick={() => onToggle(mood)}
                            onMouseEnter={() => onPreview(mood)}
                            onFocus={() => onPreview(mood)}
                            onBlur={() => onPreview(null)}
                            className="group flex shrink-0 items-center focus-visible:outline-none"
                        >
                            <span
                                className="flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px] transition-colors group-focus-visible:ring-2 group-focus-visible:ring-white/40"
                                style={{
                                    borderColor: on ? `rgb(${r} ${g} ${b} / 0.7)` : "rgba(255,255,255,0.1)",
                                    background: on ? `rgb(${r} ${g} ${b} / 0.16)` : "rgba(8,5,14,0.5)",
                                    color: on ? "#fff" : "rgba(255,255,255,0.72)",
                                }}
                            >
                                <span className="h-1.5 w-1.5 rounded-full" style={{ background: `rgb(${r} ${g} ${b})`, boxShadow: `0 0 6px rgb(${r} ${g} ${b} / 0.8)` }} />
                                {moodLabel(mood)}
                                <span className="tabular-nums text-white/60">{count.toLocaleString()}</span>
                            </span>
                        </button>
                    );
                })}
                {scroll && <span className="shrink-0 whitespace-nowrap pl-1.5 pr-6">{total}</span>}
            </div>
            {!scroll && <p className="mt-2">{total}</p>}
        </>
    );
}
