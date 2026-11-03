"use client";
import { useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/Button";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import {
    AlbumGroupedPreview,
    ALBUMS_PER_PAGE,
} from "@/features/organizer/components/AlbumGroupedPreview";
import { useAutotagProgress } from "../progress";
function OrganizePreview({ id }: { id: string }) {
    const [page, setPage] = useState(0);
    const run = useQuery({
        queryKey: ["organizer", "run", id],
        queryFn: () => api.getOrganizerRun(id, 0, 1),
        refetchInterval: 2000,
    });
    const groups = useQuery({
        queryKey: ["organizer", "albums", id, run.data?.state, page],
        queryFn: () =>
            api.getOrganizerRunAlbums(
                id,
                page * ALBUMS_PER_PAGE,
                ALBUMS_PER_PAGE,
            ),
        refetchInterval: 2000,
    });
    return (
        <section>
            <h3>Organization moves</h3>
            {groups.error && <p role="alert">{groups.error.message}</p>}
            {groups.data && (
                <AlbumGroupedPreview
                    groups={groups.data.albums}
                    total={groups.data.total}
                    page={page}
                    onPage={setPage}
                />
            )}
        </section>
    );
}
export function RunPanel({ id }: { id: string }) {
    const qc = useQueryClient();
    const [page, setPage] = useState(0);
    const run = useQuery({
        queryKey: ["autotag-run", id, page],
        queryFn: () => api.getAutotagRun(id, page * 200, 200),
        refetchInterval: 2000,
    });
    const progress = useAutotagProgress(undefined, id);
    const undo = useMutation({
        mutationFn: () => api.undoAutotagRun(id),
        onSuccess: () => {
            qc.invalidateQueries({ queryKey: ["autotag-run", id] });
            qc.invalidateQueries({ queryKey: ["autotag-runs"] });
            qc.invalidateQueries({ queryKey: ["autotag-album"] });
            qc.invalidateQueries({ queryKey: ["autotag-albums"] });
        },
    });
    const d = run.data;
    const conflict = (
        undo.error as (Error & { data?: { conflictAlbumId?: string } }) | null
    )?.data?.conflictAlbumId;
    return (
        <section
            className="space-y-3 rounded border border-white/20 p-3"
            aria-label="Match run"
        >
            {run.error && <p role="alert">{run.error.message}</p>}
            {d && (
                <>
                    <h3>
                        {d.artist} — {d.album}
                    </h3>
                    <p>
                        Run: <strong>{d.state}</strong> · {d.target}
                    </p>
                    <Link href={`/album?id=${encodeURIComponent(d.albumId)}`}>
                        Open album
                    </Link>
                    {progress && (
                        <p role="status">
                            {progress.job}: {progress.done} / {progress.total}{" "}
                            {progress.state}
                        </p>
                    )}
                    {d.error && <p role="alert">{d.error}</p>}
                    {d.summary && (
                        <pre className="whitespace-pre-wrap text-sm">
                            {JSON.stringify(d.summary, null, 2)}
                        </pre>
                    )}
                    {["failed", "undo_failed"].includes(d.state) && (
                        <p>
                            Recovery may need an owner decision. Resolve
                            conflicting library identities before retrying Undo;
                            no match history or overrides are cleared here.
                        </p>
                    )}
                    <Button
                        disabled={
                            undo.isPending ||
                            !["done", "failed", "undo_failed"].includes(d.state)
                        }
                        onClick={() => undo.mutate()}
                    >
                        Undo
                    </Button>
                    {undo.error && (
                        <p role="alert">
                            {undo.error.message}{" "}
                            {conflict && (
                                <Link
                                    href={`/album?id=${encodeURIComponent(conflict)}`}
                                >
                                    Conflicting album
                                </Link>
                            )}
                        </p>
                    )}
                    {d.organizerRunId && (
                        <OrganizePreview
                            key={d.organizerRunId}
                            id={d.organizerRunId}
                        />
                    )}
                    <table>
                        <thead>
                            <tr>
                                <th>Path</th>
                                <th>State</th>
                                <th>Changes / note</th>
                            </tr>
                        </thead>
                        <tbody>
                            {d.rows.map((r) => (
                                <tr key={r.id}>
                                    <td>{r.path}</td>
                                    <td>{r.state}</td>
                                    <td>
                                        {r.note}
                                        {r.changes.map((c) => (
                                            <div key={c.field}>
                                                {c.field}:{" "}
                                                {JSON.stringify(c.old)} →{" "}
                                                {JSON.stringify(c.new)}
                                            </div>
                                        ))}
                                    </td>
                                </tr>
                            ))}
                        </tbody>
                    </table>
                    <Button disabled={!page} onClick={() => setPage(page - 1)}>
                        Previous files
                    </Button>
                    <Button
                        disabled={(page + 1) * 200 >= d.total}
                        onClick={() => setPage(page + 1)}
                    >
                        Next files
                    </Button>
                </>
            )}
        </section>
    );
}
