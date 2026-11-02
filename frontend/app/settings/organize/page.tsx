"use client";

import { useState } from "react";
import Link from "next/link";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuth } from "@/lib/auth-context";
import { notify } from "@/lib/notify";
import { PageHeader } from "@/components/layout/PageHeader";
import { Button } from "@/components/ui/Button";
import { GradientSpinner } from "@/components/ui/GradientSpinner";
import { AlbumGroupedPreview, ALBUMS_PER_PAGE } from "@/features/organizer/components/AlbumGroupedPreview";
import { ProgressBar } from "@/features/organizer/components/ProgressBar";
import { NotAvailable } from "@/features/organizer/components/NotAvailable";
import { runActions } from "@/features/organizer/runActions";
import { clearLibraryProgress } from "@/features/organizer/progress";
import type { OrganizerMode } from "@/features/organizer/types";

const BUSY = ["planning", "applying", "undoing"];

function errorMessage(e: unknown): string {
    return e instanceof Error && e.message ? e.message : "Something went wrong";
}

export default function OrganizePage() {
    const { user } = useAuth();
    const isAdmin = user?.role === "admin";
    const qc = useQueryClient();
    const [mode, setMode] = useState<OrganizerMode>("copy");
    const [runId, setRunId] = useState<string | null>(null);
    const [pagination, setPagination] = useState({ runId: "", page: 0 });

    const caps = useQuery({
        queryKey: ["organizer", "capabilities"],
        queryFn: () => api.getOrganizerCapabilities(),
        enabled: isAdmin,
    });
    const watch = useQuery({
        queryKey: ["library", "watch-status"],
        queryFn: () => api.getWatchStatus(),
        enabled: isAdmin,
    });
    const runs = useQuery({
        queryKey: ["organizer", "runs"],
        queryFn: () => api.getOrganizerRuns(),
        enabled: isAdmin,
    });
    // Without a choice, show the newest run, so a reload keeps the last preview in view.
    const activeId = runId ?? runs.data?.runs[0]?.id ?? null;

    const page = pagination.runId === activeId ? pagination.page : 0;

    const run = useQuery({
        queryKey: ["organizer", "run", activeId],
        queryFn: () => api.getOrganizerRun(activeId!, 0, 1),
        enabled: isAdmin && !!activeId,
        refetchInterval: (q) => (q.state.data && BUSY.includes(q.state.data.state) ? 2000 : false),
    });

    const albums = useQuery({
        queryKey: ["organizer", "albums", activeId, run.data?.state, page],
        queryFn: () => api.getOrganizerRunAlbums(activeId!, page * ALBUMS_PER_PAGE, ALBUMS_PER_PAGE),
        enabled: isAdmin && !!activeId,
        refetchInterval: run.data && BUSY.includes(run.data.state) ? 2000 : false,
    });

    const refresh = () => {
        qc.invalidateQueries({ queryKey: ["organizer"] });
    };

    const start = useMutation({
        onMutate: () => clearLibraryProgress(qc, "organize"),
        mutationFn: async (action: "preview" | "apply" | "undo" | "switch") => {
            if (action === "preview") return api.previewOrganizer(mode);
            const id = run.data!.id;
            if (action === "apply") return api.applyOrganizerRun(id);
            if (action === "undo") return api.undoOrganizerRun(id);
            return api.switchOrganizerRoot(id);
        },
        onSuccess: (res, action) => {
            if (action === "preview") {
                setRunId(res.runId);
                setPagination({ runId: res.runId, page: 0 });
            }
            refresh();
        },
        onError: (e) => notify.error(errorMessage(e)),
    });

    if (!isAdmin) {
        return <NotAvailable title="Admins only" description="Only an administrator can organize the library." />;
    }

    const c = caps.data;
    const d = run.data;
    const busy = !!d && BUSY.includes(d.state);
    const modeOk = mode === "copy" ? c?.copyAvailable : c?.moveAvailable;
    const act = d ? runActions(d, busy) : null;
    const progressJob = "organize" as const;

    return (
        <div className="min-h-screen pb-32">
            <PageHeader
                eyebrow="Settings"
                title="Organize library"
                description="Preview a tidy on-disk layout, then apply it. Nothing changes until you press Apply."
                actions={
                    <Link href="/settings/attention" className="text-sm text-[var(--text-secondary)] hover:text-white">
                        Needs attention
                    </Link>
                }
            />
            <div className="page-shell section-stack">
                <section className="space-y-2 text-sm text-[var(--text-secondary)]">
                    {caps.isLoading ? (
                        <GradientSpinner size="sm" />
                    ) : c ? (
                        <>
                            <p>
                                Library folder: <span className="text-white">{c.libraryRoot ?? "not configured"}</span>
                            </p>
                            <p>
                                Copy mode:{" "}
                                {c.copyAvailable ? `available, into ${c.targetRoot}` : "unavailable (set KIMA_ORGANIZE_TARGET to a writable folder)"}
                            </p>
                            <p>Move mode: {c.moveAvailable ? "available" : "unavailable (the library folder is read-only)"}</p>
                        </>
                    ) : (
                        <p className="text-danger">Could not load organizer capabilities.</p>
                    )}
                    {watch.data && (
                        <p className="text-[var(--text-muted)]">
                            Folder watching: {watch.data.mode}
                            {watch.data.active ? " (active)" : " (inactive)"}
                            {watch.data.lastError ? `, last error: ${watch.data.lastError}` : ""}
                            {". "}
                            {watch.data.nextSyncAt
                                ? `Next safety-net sync: ${new Date(watch.data.nextSyncAt).toLocaleString()}`
                                : "Scheduled sync off"}
                        </p>
                    )}
                </section>

                <section className="flex flex-wrap items-center gap-3">
                    <label className="text-sm text-[var(--text-secondary)]" htmlFor="organize-mode">
                        Mode
                    </label>
                    <select
                        id="organize-mode"
                        value={mode}
                        onChange={(e) => setMode(e.target.value as OrganizerMode)}
                        className="rounded border border-white/10 bg-[var(--bg-primary)] px-3 py-2 text-sm text-white"
                    >
                        <option value="copy">Copy to the organized folder</option>
                        <option value="move">Move files in place</option>
                    </select>
                    <Button
                        variant="primary"
                        onClick={() => start.mutate("preview")}
                        disabled={!modeOk || busy || start.isPending}
                    >
                        Preview
                    </Button>
                </section>

                {mode === "move" && (
                    <p role="alert" className="rounded border border-danger/40 bg-danger/10 p-3 text-sm text-danger-light">
                        Move mode renames the files in your music folder on disk. Other programs that point at the old
                        paths will lose track of them. You can undo a move from the journal.
                    </p>
                )}

                {d && (
                    <section className="space-y-4">
                        <div className="flex flex-wrap items-center gap-3 text-sm">
                            <span className="text-white">
                                {d.mode === "copy" ? "Copy" : "Move"} run:{" "}
                                {d.state === "superseded" ? "Out of date — preview again" : d.state}
                            </span>
                            <span className="text-[var(--text-muted)] tabular-nums">{d.total} files</span>
                            {d.switchedAt && <span className="text-brand">Library switched to this copy</span>}
                            {d.error && <span className="text-danger">{d.error}</span>}
                        </div>

                        {busy && <ProgressBar job={progressJob} label={d.state === "planning" ? "Planning" : "Organizing"} />}

                        <div className="flex flex-wrap gap-3">
                            <Button variant="primary" onClick={() => start.mutate("apply")} disabled={!act?.canApply || start.isPending}>
                                {act?.applyLabel}
                            </Button>
                            <Button variant="secondary" onClick={() => start.mutate("undo")} disabled={!act?.canUndo || start.isPending}>
                                {act?.undoLabel}
                            </Button>
                            {d.mode === "copy" && (
                                <Button
                                    variant="secondary"
                                    onClick={() => start.mutate("switch")}
                                    disabled={!act?.canSwitch || start.isPending}
                                >
                                    Switch library to the organized copy
                                </Button>
                            )}
                        </div>

                        {albums.isLoading && <GradientSpinner size="sm" />}
                        {albums.isError && <p role="alert" className="text-danger">Could not load album preview.</p>}
                        {albums.data && (
                            <AlbumGroupedPreview
                                key={d.id}
                                groups={albums.data.albums}
                                page={page}
                                total={albums.data.total}
                                onPage={(n) => setPagination({ runId: d.id, page: n })}
                            />
                        )}
                    </section>
                )}
            </div>
        </div>
    );
}
