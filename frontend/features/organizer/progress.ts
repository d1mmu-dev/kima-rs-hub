import type { QueryClient } from "@tanstack/react-query";
import type { LibraryProgress } from "./types";

export type LibraryJob = "scan" | "organize";

export const libraryProgressKey = (job: LibraryJob) => ["library-progress", job] as const;

/** Handles a `library:progress` SSE event: stores `{done, total}` for its job. */
export function applyLibraryProgress(queryClient: QueryClient, data: unknown): void {
    const e = data as { job?: string; done?: unknown; total?: unknown } | null;
    if (!e || (e.job !== "scan" && e.job !== "organize")) return;
    if (typeof e.done !== "number" || typeof e.total !== "number") return;
    queryClient.setQueryData<LibraryProgress>(libraryProgressKey(e.job), { done: e.done, total: e.total });
}

/** Forgets a job's progress, so a finished run's 100% never shows for the next one. */
export function clearLibraryProgress(queryClient: QueryClient, job: LibraryJob): void {
    queryClient.setQueryData<LibraryProgress | null>(libraryProgressKey(job), null);
}

/** Whole-number percentage 0-100; 0 while the total is unknown. */
export function progressPercent(p: LibraryProgress | undefined | null): number {
    if (!p || p.total <= 0) return 0;
    return Math.max(0, Math.min(100, Math.round((p.done / p.total) * 100)));
}
