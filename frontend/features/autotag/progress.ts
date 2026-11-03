import { useQuery, skipToken, type QueryClient } from "@tanstack/react-query";
export interface AutotagProgress {
    job: "search" | "apply" | "undo";
    albumId?: string;
    runId?: string;
    done: number;
    total: number;
    state?: string;
}
export function applyAutotagProgress(qc: QueryClient, payload: unknown) {
    if (!payload || typeof payload !== "object") return;
    const data = payload as AutotagProgress;
    if (
        !["search", "apply", "undo"].includes(data.job) ||
        typeof data.done !== "number" ||
        !Number.isFinite(data.done) ||
        data.done < 0 ||
        typeof data.total !== "number" ||
        !Number.isFinite(data.total) ||
        data.total < 0 ||
        (data.albumId !== undefined && typeof data.albumId !== "string") ||
        (data.runId !== undefined && typeof data.runId !== "string")
    )
        return;
    qc.setQueryData(["autotag-progress"], data);
    if (data.runId) {
        qc.setQueryData(["autotag-progress", data.runId], data);
        qc.invalidateQueries({ queryKey: ["autotag-run", data.runId] });
    }
    if (data.albumId) {
        qc.setQueryData(["autotag-progress", "album", data.albumId], data);
        qc.invalidateQueries({ queryKey: ["autotag-album", data.albumId] });
    } else qc.invalidateQueries({ queryKey: ["autotag-album"] });
    qc.invalidateQueries({ queryKey: ["autotag-albums"] });
    qc.invalidateQueries({ queryKey: ["autotag-runs"] });
    qc.invalidateQueries({ queryKey: ["album"] });
}
export function useAutotagProgress(albumId?: string, runId?: string) {
    return useQuery<AutotagProgress>({
        queryKey: runId
            ? ["autotag-progress", runId]
            : albumId
              ? ["autotag-progress", "album", albumId]
              : ["autotag-progress"],
        queryFn: skipToken,
        enabled: false,
    }).data;
}
