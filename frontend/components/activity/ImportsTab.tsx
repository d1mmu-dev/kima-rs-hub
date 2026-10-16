"use client";

import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import {
    ListMusic,
    Loader2,
    X,
    CheckCircle2,
    AlertCircle,
    ExternalLink,
} from "lucide-react";
import { api } from "@/lib/api";
import { cn } from "@/lib/cn";
import { EmptyState } from "../ui/EmptyState";
import { GradientSpinner } from "../ui/GradientSpinner";
import { useAuth } from "@/lib/auth-context";
import { timeAgo } from "@/lib/relative-time";

interface ImportJob {
    id: string;
    playlistName: string;
    status: string;
    progress: number;
    albumsTotal: number;
    albumsCompleted: number;
    tracksMatched: number;
    tracksTotal: number;
    tracksDownloadable: number;
    createdPlaylistId: string | null;
    error: string | null;
    createdAt: string;
}

const ACTIVE_STATUSES = [
    "pending",
    "fetching",
    "downloading",
    "scanning",
    "creating_playlist",
    "matching_tracks",
];

function statusLabel(status: string): string {
    switch (status) {
        case "pending":
            return "Pending";
        case "fetching":
            return "Fetching";
        case "downloading":
            return "Downloading";
        case "scanning":
            return "Scanning";
        case "creating_playlist":
            return "Creating";
        case "matching_tracks":
            return "Matching";
        case "completed":
            return "Done";
        case "failed":
            return "Failed";
        case "cancelled":
            return "Cancelled";
        default:
            // An unmapped status from the server, shown as-is rather than
            // stamped into capitals.
            return status.charAt(0).toUpperCase() + status.slice(1).replace(/_/g, " ");
    }
}

function statusColor(status: string): string {
    if (ACTIVE_STATUSES.includes(status)) return "text-brand";
    if (status === "completed") return "text-success";
    if (status === "failed") return "text-danger-light";
    if (status === "cancelled") return "text-warning";
    return "text-[var(--text-muted)]";
}

export function ImportsTab() {
    const { isAuthenticated } = useAuth();
    const queryClient = useQueryClient();
    const [cancelling, setCancelling] = useState<Set<string>>(new Set());

    const { data: imports = [], isLoading } = useQuery<ImportJob[]>({
        queryKey: ["user-imports"],
        queryFn: () => api.get<ImportJob[]>("/spotify/imports"),
        enabled: isAuthenticated,
        refetchInterval: 10000,
        refetchIntervalInBackground: false,
    });

    const handleCancel = async (jobId: string) => {
        setCancelling((prev) => new Set(prev).add(jobId));
        try {
            await api.post(`/spotify/import/${jobId}/cancel`);
            queryClient.invalidateQueries({ queryKey: ["user-imports"] });
        } catch (error) {
            console.error("Failed to cancel import:", error);
        } finally {
            setCancelling((prev) => {
                const next = new Set(prev);
                next.delete(jobId);
                return next;
            });
        }
    };

    if (isLoading) {
        return (
            <div className="flex items-center justify-center py-8">
                <GradientSpinner size="sm" />
            </div>
        );
    }

    if (imports.length === 0) {
        return (
            <EmptyState
                icon={<ListMusic className="w-8 h-8" />}
                title="No imports yet"
                description="Playlist imports will appear here"
            />
        );
    }

    const activeImports = imports.filter((j) =>
        ACTIVE_STATUSES.includes(j.status),
    );
    const pastImports = imports.filter(
        (j) => !ACTIVE_STATUSES.includes(j.status),
    );

    return (
        <div className="flex flex-col h-full">
            {activeImports.length > 0 && (
                <div className="flex items-center justify-between px-3 py-2 border-b-2 border-white/10">
                    <div className="flex items-center gap-2">
                        <span className="w-1.5 h-1.5 rounded-full bg-brand animate-pulse" />
                        <span className="t-eyebrow">
                            {String(activeImports.length).padStart(2, "0")}{" "}
                            ACTIVE
                        </span>
                    </div>
                </div>
            )}

            <div className="flex-1 overflow-y-auto">
                {activeImports.map((job, index) => (
                    <ImportJobCard
                        key={job.id}
                        job={job}
                        index={index}
                        cancelling={cancelling.has(job.id)}
                        onCancel={handleCancel}
                    />
                ))}

                {pastImports.length > 0 && activeImports.length > 0 && (
                    <div className="flex items-center gap-2 px-3 py-2 border-b border-white/5">
                        <span className="text-xs font-semibold text-[var(--text-muted)]">
                            Previous
                        </span>
                        <span className="flex-1 border-t border-white/5" />
                    </div>
                )}

                {pastImports.map((job, index) => (
                    <ImportJobCard
                        key={job.id}
                        job={job}
                        index={activeImports.length + index}
                        cancelling={false}
                        onCancel={handleCancel}
                    />
                ))}
            </div>
        </div>
    );
}

function ImportJobCard({
    job,
    index,
    cancelling,
    onCancel,
}: {
    job: ImportJob;
    index: number;
    cancelling: boolean;
    onCancel: (id: string) => void;
}) {
    const isActive = ACTIVE_STATUSES.includes(job.status);
    const borderColor = isActive
        ? "border-brand"
        : job.status === "completed"
          ? "border-success"
          : job.status === "failed"
            ? "border-danger-light"
            : "border-white/5";

    return (
        <div
            className={cn(
                "px-3 py-3 border-b border-white/5 border-l-2 bg-[var(--bg-secondary)] hover:bg-white/5 transition-colors group",
                borderColor,
            )}
        >
            <div className="flex items-start gap-3">
                <div className="flex-shrink-0 w-6 mt-0.5">
                    <span className="text-micro tabular-nums font-semibold text-[var(--text-muted)]">
                        {String(index + 1).padStart(2, "0")}
                    </span>
                </div>

                <div className="mt-0.5 shrink-0">
                    {isActive ? (
                        cancelling ? (
                            <Loader2 className="w-4 h-4 text-[var(--text-muted)] animate-spin" />
                        ) : (
                            <GradientSpinner size="sm" />
                        )
                    ) : job.status === "completed" ? (
                        <CheckCircle2 className="w-4 h-4 text-success" />
                    ) : (
                        <AlertCircle className="w-4 h-4 text-danger-light" />
                    )}
                </div>

                <div className="flex-1 min-w-0">
                    <p className="text-xs font-semibold tracking-tight text-white truncate mb-1">
                        {job.playlistName}
                    </p>

                    <div className="flex items-center gap-2 flex-wrap">
                        <span
                            className={cn(
                                "text-xs font-semibold",
                                statusColor(job.status),
                            )}
                        >
                            {statusLabel(job.status)}
                        </span>

                        {isActive && job.albumsTotal > 0 && (
                            <>
                                <span className="text-micro text-[var(--text-muted)]">
                                    --
                                </span>
                                <span className="text-xs tabular-nums text-[var(--text-muted)]">
                                    {job.albumsCompleted}/{job.albumsTotal}{" "}
                                    albums
                                </span>
                            </>
                        )}

                        {job.tracksMatched > 0 && (
                            <>
                                <span className="text-micro text-[var(--text-muted)]">
                                    --
                                </span>
                                <span className="text-xs tabular-nums text-[var(--text-muted)]">
                                    {job.tracksMatched} tracks matched
                                </span>
                            </>
                        )}

                        <span className="text-micro text-[var(--text-muted)]">
                            --
                        </span>
                        <span className="text-xs tabular-nums text-[var(--text-muted)]">
                            {timeAgo(job.createdAt)}
                        </span>
                    </div>

                    {/* Progress bar for active imports */}
                    {isActive && job.albumsTotal > 0 && (
                        <div className="mt-2 h-1 bg-white/5 rounded-full overflow-hidden">
                            <div
                                className="h-full bg-brand rounded-full transition-all duration-300"
                                style={{
                                    width: `${Math.round((job.albumsCompleted / job.albumsTotal) * 100)}%`,
                                }}
                            />
                        </div>
                    )}

                    {job.status === "failed" && job.error && (
                        <p className="text-micro tabular-nums text-danger-light/70 mt-1 truncate">
                            {job.error}
                        </p>
                    )}

                    {/* Completed: link to playlist */}
                    {job.status === "completed" && job.createdPlaylistId && (
                        <Link
                            href={`/playlist/${job.createdPlaylistId}`}
                            className="inline-flex items-center gap-1 mt-1.5 text-micro tabular-nums font-semibold text-success hover:text-success/80 transition-colors"
                        >
                            <ExternalLink className="w-3 h-3" />
                            View Playlist
                        </Link>
                    )}
                </div>

                {/* Cancel button for active imports */}
                {isActive && (
                    <button
                        onClick={() => onCancel(job.id)}
                        disabled={cancelling}
                        className="p-1 hover:bg-white/10 transition-colors shrink-0"
                        title="cancel import"
                    >
                        <X className="w-3 h-3 text-[var(--text-muted)] hover:text-danger-light" />
                    </button>
                )}
            </div>
        </div>
    );
}
