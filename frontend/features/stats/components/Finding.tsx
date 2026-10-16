"use client";

/**
 * A claim in plain words with its evidence underneath, for the few facts a
 * reader might dispute. The one-sentence note shares the claim's line; the
 * rule beneath the pair marks where the proof starts.
 */

import { Play } from "lucide-react";
import { Button } from "@/components/ui/Button";
import { usePlaySlice } from "../usePlaySlice";

export function Finding({
    claim,
    note,
    evidence,
    scope,
}: {
    /** The thing being asserted, in plain words. The loudest element. */
    claim: React.ReactNode;
    /** One sentence of context. Never the number the claim already stated. */
    note?: React.ReactNode;
    /** The graphic that proves it. */
    evidence?: React.ReactNode;
    /** Names the period a shelf section is about, for sections the period control does not reach. */
    scope?: string;
}) {
    return (
        <div className="flex flex-col gap-(--stat-gap-block)">
            <div className="flex flex-wrap items-baseline justify-between gap-x-10 gap-y-2 border-b border-white/[0.08] pb-4">
                <h2 className="text-[length:var(--stat-claim)] leading-[1.2] font-light tracking-[-0.02em] text-[var(--text-primary)] max-w-[26ch] text-balance">
                    {claim}
                    {scope && <span className="ml-3 t-meta align-middle">{scope}</span>}
                </h2>
                {note && <p className="t-body max-w-[42ch] lg:text-right">{note}</p>}
            </div>

            {evidence}
        </div>
    );
}

/** Mirrors `MAX_TRACKS` in backend/src/services/stats/playable.ts; scripts/verify-stats.ts fails if they drift. */
const SLICE_CAP = 100;

export function PlayCore({ count, period }: { count: number; period?: string }) {
    const { play, busy } = usePlaySlice();
    const willPlay = Math.min(count, SLICE_CAP);
    return (
        <Button
            variant="ghost"
            onClick={() => void play("core", { limit: willPlay, period })}
            isLoading={busy === "core"}
            className="self-start -ml-3.5"
        >
            {busy !== "core" && <Play className="w-3.5 h-3.5" />}
            {/* Says what will happen, not what was hoped for. */}
            {willPlay < count ? `play the top ${willPlay}` : `play these ${count}`}
        </Button>
    );
}
