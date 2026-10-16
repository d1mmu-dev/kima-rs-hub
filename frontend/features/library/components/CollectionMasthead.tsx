"use client";

/**
 * The collection's masthead: the title, one line on how much of the library
 * has been played, and a thin band split by each mood's share of it in the
 * vibe map's colours, under a soft violet bleed from the top-right.
 */

import { MOOD_COLORS, moodLabel, type MoodShare } from "@/features/vibe/mapUtils";

interface CollectionMastheadProps {
    title: string;
    /** The stats line; null until it loads, when `fallbackLine` shows instead. */
    heardLine: string | null;
    /** The active tab's count, e.g. "315 artists". */
    fallbackLine: string;
    shares: MoodShare[];
}

export function CollectionMasthead({ title, heardLine, fallbackLine, shares }: CollectionMastheadProps) {
    return (
        <header className="relative">
            {/* The cosmos bleed: soft violet from the top-right, reaching up past the column. */}
            <div
                aria-hidden="true"
                className="pointer-events-none absolute inset-x-0 -top-16 h-[300px]"
                style={{
                    background: "radial-gradient(280px 280px at 82% 14%, rgba(119, 59, 151, 0.30), transparent 70%)",
                }}
            />

            <div className="relative page-shell pt-8 pb-6">
                <h1 className="text-4xl md:text-5xl font-bold tracking-tight text-white leading-none">{title}</h1>
                <p className="mt-3 text-sm tabular-nums text-[var(--text-secondary)] truncate transition-opacity duration-500">
                    {heardLine ?? fallbackLine}
                </p>
                <MoodBand shares={shares} />
            </div>
        </header>
    );
}

/** One 4px band, each mood's stretch as wide as its share; a quiet track until the map loads. */
function MoodBand({ shares }: { shares: MoodShare[] }) {
    const label = shares.length
        ? "moods: " + shares.map((s) => `${moodLabel(s.key).toLowerCase()} ${Math.round(s.fraction * 100)}%`).join(", ")
        : undefined;

    return (
        <div
            role={shares.length ? "img" : undefined}
            aria-label={label}
            className="mt-3 h-1 w-full overflow-hidden rounded-full"
            style={{ background: "rgba(255, 240, 214, 0.06)" }}
        >
            <div className="flex h-full w-full">
                {shares.map((share) => {
                    const colour = MOOD_COLORS[share.key] ?? MOOD_COLORS.neutral;
                    return (
                        <div
                            key={share.key}
                            className="h-full transition-[width] duration-500"
                            style={{
                                width: `${share.fraction * 100}%`,
                                background: `rgb(${colour[0]} ${colour[1]} ${colour[2]})`,
                            }}
                        />
                    );
                })}
            </div>
        </div>
    );
}
