"use client";

import { useQuery } from "@tanstack/react-query";
import { libraryProgressKey, progressPercent, type LibraryJob } from "../progress";
import type { LibraryProgress } from "../types";

/** Reads the latest `library:progress` event for a job (the SSE hook writes it to the query cache). */
export function ProgressBar({ job, label }: { job: LibraryJob; label: string }) {
    const { data } = useQuery<LibraryProgress | null>({
        queryKey: libraryProgressKey(job),
        queryFn: () => null,
        enabled: false,
        staleTime: Infinity,
    });
    const pct = progressPercent(data);
    return (
        <div>
            <div className="mb-1 flex justify-between text-xs text-[var(--text-muted)] tabular-nums">
                <span>{label}</span>
                <span>{data && data.total > 0 ? `${data.done} / ${data.total}` : "waiting"}</span>
            </div>
            <div
                role="progressbar"
                aria-label={label}
                aria-valuemin={0}
                aria-valuemax={100}
                aria-valuenow={pct}
                className="h-2 w-full overflow-hidden rounded bg-white/10"
            >
                <div className="h-full bg-brand transition-all duration-200" style={{ width: `${pct}%` }} />
            </div>
        </div>
    );
}
