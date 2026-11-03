"use client";
import { Suspense, useEffect, useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/Button";
import { useSearchParams } from "next/navigation";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuth } from "@/lib/auth-context";
import { PageHeader } from "@/components/layout/PageHeader";
import { NotAvailable } from "@/features/organizer/components/NotAvailable";
import { CandidateList } from "@/features/autotag/components/CandidateList";
import { ChangesTable } from "@/features/autotag/components/ChangesTable";
import { MappingDiff } from "@/features/autotag/components/MappingDiff";
import { TargetPicker } from "@/features/autotag/components/TargetPicker";
import { RunPanel } from "@/features/autotag/components/RunPanel";
import { useAutotagProgress } from "@/features/autotag/progress";
import type { Search, Target } from "@/features/autotag/types";
export default function ReviewPage() {
    return (
        <Suspense fallback={<p>Loading…</p>}>
            <ReviewRoute />
        </Suspense>
    );
}
function ReviewRoute() {
    const id = useSearchParams().get("albumId") ?? "";
    return id ? (
        <ReviewAlbum key={id} id={id} />
    ) : (
        <p role="alert">Choose an album from Match albums.</p>
    );
}
function ReviewAlbum({ id }: { id: string }) {
    const { user } = useAuth();
    const admin = user?.role === "admin";
    const qc = useQueryClient();
    const [candidateId, setCandidate] = useState<string>();
    const [searchGeneration, setSearchGeneration] = useState(0);
    const [target, setTarget] = useState<Target>("db");
    const caps = useQuery({
        queryKey: ["autotag-capabilities"],
        queryFn: () => api.getAutotagCapabilities(),
        enabled: admin,
    });
    const review = useQuery({
        queryKey: ["autotag-album", id, searchGeneration, candidateId],
        queryFn: () => api.getAutotagAlbum(id, candidateId),
        enabled: admin && !!id,
        refetchInterval: 2000,
    });
    useEffect(() => {
        if (
            candidateId &&
            (review.isError ||
                (review.data &&
                    !review.data.candidates.some((c) => c.id === candidateId)))
        ) {
            // Reconcile local selection with the asynchronously updated server review.
            // eslint-disable-next-line react-hooks/set-state-in-effect
            setCandidate(undefined);
        }
    }, [candidateId, review.isError, review.data]);
    const refresh = () => {
        qc.invalidateQueries({ queryKey: ["autotag-album", id] });
        qc.invalidateQueries({ queryKey: ["autotag-runs"] });
        qc.invalidateQueries({ queryKey: ["autotag-albums"] });
    };
    const [artist, setArtist] = useState<string>();
    const [album, setAlbum] = useState<string>();
    const [release, setRelease] = useState("");
    const search = useMutation({
        mutationFn: (body: Search) => api.searchAutotag(body),
        onSuccess: () => {
            setCandidate(undefined);
            setSearchGeneration((generation) => generation + 1);
            refresh();
        },
    });
    const skip = useMutation({
        mutationFn: () => api.skipAutotagAlbum(id),
        onSuccess: refresh,
    });
    const [runId, setRunId] = useState<string>();
    const progress = useAutotagProgress(id);
    const [reorganize, setReorganize] = useState(false);
    const apply = useMutation({
        mutationFn: (mbidsOnly: boolean) =>
            api.applyAutotagAlbum(id, {
                candidateId: review.data!.selected!.candidateId,
                target: mbidsOnly ? "db" : target,
                reorganize: !mbidsOnly && target === "files" && reorganize,
                mbidsOnly,
            }),
        onSuccess: (r) => {
            setRunId(r.runId);
            refresh();
        },
    });
    if (!admin)
        return (
            <NotAvailable
                title="Admins only"
                description="Only an administrator can match albums."
            />
        );
    const applyConflict = (
        apply.error as (Error & { data?: { conflictAlbumId?: string } }) | null
    )?.data?.conflictAlbumId;
    const d = review.data,
        s = d?.selected;
    const selectedCandidate = d?.candidates.find((c) => c.id === s?.candidateId);
    const unsafeReview =
        review.isFetching || review.isError || !s || !selectedCandidate || selectedCandidate.stale;
    const busy =
        apply.isPending ||
        skip.isPending ||
        search.isPending ||
        ["queued", "searching", "applying", "applied"].includes(d?.state ?? "");
    return (
        <div className="min-h-screen pb-32">
            <PageHeader eyebrow="Settings" title="Review album match" />
            <main className="page-shell section-stack">
                <Link href="/settings/autotag">Match albums</Link>
                {review.isLoading && <p>Loading…</p>}
                {review.error && <p role="alert">{review.error.message}</p>}
                {caps.error && <p role="alert">{caps.error.message}</p>}
                {progress && (
                    <p role="status">
                        {progress.job}: {progress.done} / {progress.total}{" "}
                        {progress.state}
                    </p>
                )}
                {runId && <RunPanel key={runId} id={runId} />}{" "}
                {d && (
                    <>
                        <h2>
                            {d.album.artist} — {d.album.title}
                        </h2>
                        <Link href={`/album?id=${encodeURIComponent(id)}`}>
                            Open album
                        </Link>
                        <p>{d.state}</p>
                        {d.error && <p role="alert">{d.error}</p>}
                        <section>
                            <h2>Search MusicBrainz</h2>
                            {!caps.data?.identifyEnabled && (
                                <p>
                                    Outbound lookups are turned off. Stored
                                    fresh candidates can still be applied.
                                </p>
                            )}
                            <label>
                                Artist
                                <input
                                    aria-label="Search artist"
                                    value={artist ?? d.album.artist}
                                    onChange={(e) => setArtist(e.target.value)}
                                />
                            </label>
                            <label>
                                Album
                                <input
                                    aria-label="Search album"
                                    value={album ?? d.album.title}
                                    onChange={(e) => setAlbum(e.target.value)}
                                />
                            </label>
                            <Button
                                disabled={
                                    busy ||
                                    !caps.data?.identifyEnabled ||
                                    !(artist ?? d.album.artist).trim() ||
                                    !(album ?? d.album.title).trim()
                                }
                                onClick={() =>
                                    search.mutate({
                                        albumIds: [id],
                                        query: {
                                            artist: artist ?? d.album.artist,
                                            album: album ?? d.album.title,
                                        },
                                    })
                                }
                            >
                                Search artist / album
                            </Button>
                            <label>
                                Release MBID or URL
                                <input
                                    aria-label="Release MBID or URL"
                                    value={release}
                                    onChange={(e) => setRelease(e.target.value)}
                                />
                            </label>
                            <Button
                                disabled={
                                    busy ||
                                    !caps.data?.identifyEnabled ||
                                    !release.trim()
                                }
                                onClick={() =>
                                    search.mutate({
                                        albumIds: [id],
                                        releaseMbid: release,
                                    })
                                }
                            >
                                Find release
                            </Button>
                        </section>
                        <CandidateList
                            candidates={d.candidates}
                            selected={s?.candidateId}
                            onSelect={setCandidate}
                        />
                        {d.candidates.find((c) => c.id === s?.candidateId)
                            ?.stale && (
                            <Button
                                disabled={busy || !caps.data?.identifyEnabled}
                                onClick={() =>
                                    search.mutate({ albumIds: [id] })
                                }
                            >
                                Refresh candidates
                            </Button>
                        )}
                        {selectedCandidate?.stale && (
                            <p>Refresh this stale candidate before applying it.</p>
                        )}
                        {d.userMbidKept && (
                            <p>Your user-set MusicBrainz IDs will be kept.</p>
                        )}
                        {d.conflict && (
                            <section role="alert">
                                <p>
                                    Conflicts with{" "}
                                    <Link
                                        href={`/album?id=${encodeURIComponent(d.conflict.albumId)}`}
                                    >
                                        {d.conflict.artist} — {d.conflict.title}
                                    </Link>
                                </p>
                                <Button
                                    disabled={busy || unsafeReview}
                                    onClick={() => apply.mutate(true)}
                                >
                                    Apply MBIDs only
                                </Button>
                                <Button disabled>Merge (later)</Button>
                            </section>
                        )}
                        {s && (
                            <>
                                <ChangesTable changes={s.changes} />
                                <MappingDiff
                                    mapping={s.mapping}
                                    missing={s.missing}
                                    extra={s.extra}
                                    tracks={d.album.tracks}
                                />
                                {caps.data && (
                                    <TargetPicker
                                        capabilities={caps.data}
                                        value={target}
                                        onChange={setTarget}
                                    />
                                )}
                                <label>
                                    <input
                                        aria-label="Re-organize after writing"
                                        type="checkbox"
                                        checked={
                                            target === "files" && reorganize
                                        }
                                        disabled={target !== "files" || busy}
                                        onChange={(e) =>
                                            setReorganize(e.target.checked)
                                        }
                                    />{" "}
                                    Re-organize after writing
                                </label>
                                {target === "copy" && (
                                    <p>
                                        Undo this tag write before undoing the
                                        copy run
                                    </p>
                                )}
                                <Button
                                    disabled={
                                        busy ||
                                        unsafeReview ||
                                        !!d.conflict ||
                                        !caps.data?.[target].available
                                    }
                                    onClick={() => apply.mutate(false)}
                                >
                                    Accept
                                </Button>
                            </>
                        )}
                        <Button disabled={busy} onClick={() => skip.mutate()}>
                            Skip
                        </Button>
                        {search.error && (
                            <p role="alert">{search.error.message}</p>
                        )}
                        {skip.error && <p role="alert">{skip.error.message}</p>}
                        {apply.error && (
                            <p role="alert">
                                {apply.error.message}{" "}
                                {applyConflict && (
                                    <Link
                                        href={`/album?id=${encodeURIComponent(applyConflict)}`}
                                    >
                                        Conflicting album
                                    </Link>
                                )}
                            </p>
                        )}
                    </>
                )}
            </main>
        </div>
    );
}
