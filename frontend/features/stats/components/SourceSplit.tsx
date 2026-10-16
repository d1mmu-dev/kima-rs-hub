"use client";

/**
 * Where plays came from: your shelf, Discover, kept-from-Discover, Subsonic
 * clients. One bar, four segments; the Discover pair sit together so kept
 * reads against offered.
 */

import type { ListeningStats } from "../types";

const ORDER = ["LIBRARY", "SUBSONIC", "DISCOVERY", "DISCOVERY_KEPT"] as const;

const SAY: Record<string, string> = {
    LIBRARY: "library",
    SUBSONIC: "subsonic apps",
    DISCOVERY: "discover",
    DISCOVERY_KEPT: "discover, kept",
};

/** Opacity, not hue. The accent stays one colour and share does the work. */
const INK: Record<string, string> = {
    LIBRARY: "100%",
    SUBSONIC: "62%",
    DISCOVERY: "34%",
    DISCOVERY_KEPT: "80%",
};

export function SourceSplit({ listening }: { listening: ListeningStats }) {
    const { bySource, plays } = listening;
    const segments = ORDER.map((k) => ({ key: k, n: bySource[k] ?? 0 })).filter((s) => s.n > 0);
    if (plays === 0 || segments.length === 0) return null;

    // A bar of one segment at a hundred per cent says nothing, so one source
    // over 97% gets one line instead.
    const dominant = segments.reduce((a, b) => (b.n > a.n ? b : a));
    if (dominant.n / plays >= 0.97) {
        return (
            <p className="t-body max-w-[44ch]">
                all plays from{" "}
                <span className="text-[var(--text-primary)]">{SAY[dominant.key]}</span>
            </p>
        );
    }

    const offered = (bySource.DISCOVERY ?? 0) + (bySource.DISCOVERY_KEPT ?? 0);
    const kept = bySource.DISCOVERY_KEPT ?? 0;
    const keepRate = offered > 0 ? Math.round((kept / offered) * 100) : null;

    return (
        <div className="flex flex-col gap-4">
            <div className="flex h-2 w-full gap-[2px] overflow-hidden">
                {segments.map((s) => (
                    <div
                        key={s.key}
                        style={{
                            width: `${(s.n / plays) * 100}%`,
                            background: `color-mix(in oklab, var(--color-brand) ${INK[s.key]}, transparent)`,
                        }}
                    />
                ))}
            </div>

            {/* Rows rather than a four-up figure grid. This lives in one third
                of the measure now, and four columns of numbers in 200 pixels
                is a table nobody can read. A name and a share on one line is
                the same information at a quarter of the height. */}
            <dl className="flex flex-col">
                {segments.map((s) => (
                    <div
                        key={s.key}
                        className="flex items-baseline justify-between gap-4 border-b border-white/[0.05] py-1.5 last:border-0"
                    >
                        <dt className="t-body">{SAY[s.key]}</dt>
                        <dd className="t-figure text-sm shrink-0">
                            {Math.round((s.n / plays) * 100)}
                            <span className="text-xs font-normal text-[var(--text-muted)]">%</span>
                        </dd>
                    </div>
                ))}
            </dl>

            {keepRate !== null && (
                <p className="t-meta max-w-[32ch]">
                    {offered.toLocaleString()} discover {offered === 1 ? "play" : "plays"} ·{" "}
                    <span className="t-figure text-xs">{keepRate}%</span> on albums you kept
                </p>
            )}
        </div>
    );
}
