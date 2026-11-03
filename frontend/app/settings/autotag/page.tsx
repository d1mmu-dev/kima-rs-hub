"use client";
import { useState } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/Button";
import { useQuery, useMutation } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuth } from "@/lib/auth-context";
import { PageHeader } from "@/components/layout/PageHeader";
import { NotAvailable } from "@/features/organizer/components/NotAvailable";
import { IssueChip } from "@/features/organizer/components/IssueChip";
import { RunPanel } from "@/features/autotag/components/RunPanel";
import { useAutotagProgress } from "@/features/autotag/progress";
export default function MatchAlbumsPage() {
    const { user } = useAuth();
    const admin = user?.role === "admin";
    const [selected, setSelected] = useState<string[]>([]);
    const [run, setRun] = useState<string>();
    const [state, setState] = useState("");
    const [recommendation, setRecommendation] = useState("");
    const [q, setQ] = useState("");
    const [page, setPage] = useState(0);
    const filters = {
        state: state || undefined,
        recommendation: recommendation || undefined,
        q: q || undefined,
        offset: page * 50,
        limit: 50,
    };
    const reset = () => {
        setPage(0);
        setSelected([]);
    };
    const caps = useQuery({
        queryKey: ["autotag-capabilities"],
        queryFn: () => api.getAutotagCapabilities(),
        enabled: admin,
    });
    const albums = useQuery({
        queryKey: ["autotag-albums", filters],
        queryFn: () => api.getAutotagAlbums(filters),
        enabled: admin,
        refetchInterval: 2000,
    });
    const runs = useQuery({
        queryKey: ["autotag-runs"],
        queryFn: () => api.getAutotagRuns(),
        enabled: admin,
        refetchInterval: 2000,
    });
    const search = useMutation({
        mutationFn: (all: boolean) =>
            api.searchAutotag(all ? { all: true } : { albumIds: selected }),
        onSuccess: () => albums.refetch(),
    });
    const progress = useAutotagProgress();
    if (!admin)
        return (
            <NotAvailable
                title="Admins only"
                description="Only an administrator can match albums."
            />
        );
    return (
        <div className="min-h-screen pb-32">
            <PageHeader
                eyebrow="Settings"
                title="Match albums"
                description="Review stored MusicBrainz proposals before accepting any changes."
            />
            <main className="page-shell section-stack">
                <section>
                    <h2>Capabilities</h2>
                    {caps.data && (
                        <>
                            <p>
                                Identification:{" "}
                                {caps.data.identifyEnabled
                                    ? "enabled"
                                    : "off — stored candidates can still be applied"}
                            </p>
                            {(
                                [
                                    ["files", "Write files"],
                                    ["copy", "Organized copy"],
                                    ["db", "Database only"],
                                ] as const
                            ).map(([id, label]) => (
                                <p key={id}>
                                    {label}:{" "}
                                    {caps.data[id].available
                                        ? "available"
                                        : String(
                                              "reason" in caps.data[id]
                                                  ? caps.data[id].reason
                                                  : "unavailable",
                                          )}
                                </p>
                            ))}
                        </>
                    )}
                </section>
                {[caps.error, albums.error, runs.error, search.error]
                    .filter(Boolean)
                    .map((e, i) => (
                        <p role="alert" key={i}>
                            {e!.message}
                        </p>
                    ))}
                <section>
                    <label>
                        State
                        <select
                            aria-label="State"
                            value={state}
                            onChange={(e) => {
                                setState(e.target.value);
                                reset();
                            }}
                        >
                            {[
                                "",
                                "new",
                                "queued",
                                "searching",
                                "proposed",
                                "no_candidates",
                                "failed",
                                "applying",
                                "applied",
                                "skipped",
                            ].map((s) => (
                                <option key={s} value={s}>
                                    {s || "All states"}
                                </option>
                            ))}
                        </select>
                    </label>
                    <label>
                        Recommendation
                        <select
                            aria-label="Recommendation"
                            value={recommendation}
                            onChange={(e) => {
                                setRecommendation(e.target.value);
                                reset();
                            }}
                        >
                            {["", "strong", "medium", "none"].map((s) => (
                                <option key={s} value={s}>
                                    {s || "All recommendations"}
                                </option>
                            ))}
                        </select>
                    </label>
                    <label>
                        Search albums
                        <input
                            aria-label="Search albums"
                            value={q}
                            onChange={(e) => {
                                setQ(e.target.value);
                                reset();
                            }}
                        />
                    </label>
                </section>
                <div>
                    <Button
                        disabled={
                            !selected.length ||
                            !caps.data?.identifyEnabled ||
                            search.isPending
                        }
                        onClick={() => search.mutate(false)}
                    >
                        Find matches
                    </Button>
                    <Button
                        disabled={
                            !caps.data?.identifyEnabled || search.isPending
                        }
                        onClick={() => search.mutate(true)}
                    >
                        Match all unmatched
                    </Button>
                </div>
                {progress && (
                    <p role="status">
                        {progress.job}: {progress.done} / {progress.total}{" "}
                        {progress.state}
                    </p>
                )}
                {search.data && (
                    <p role="status">Queued {search.data.queued} album(s)</p>
                )}
                <table>
                    <thead>
                        <tr>
                            <th>Select</th>
                            <th>Album</th>
                            <th>State</th>
                            <th>Recommendation</th>
                            <th>Issues</th>
                        </tr>
                    </thead>
                    <tbody>
                        {albums.data?.albums.map((a) => (
                            <tr key={a.albumId}>
                                <td>
                                    <input
                                        type="checkbox"
                                        aria-label={`Select ${a.album}`}
                                        checked={selected.includes(a.albumId)}
                                        onChange={(e) =>
                                            setSelected(
                                                e.target.checked
                                                    ? [...selected, a.albumId]
                                                    : selected.filter(
                                                          (id) =>
                                                              id !== a.albumId,
                                                      ),
                                            )
                                        }
                                    />
                                </td>
                                <td>
                                    <Link
                                        href={`/settings/autotag/album/?albumId=${encodeURIComponent(a.albumId)}`}
                                    >
                                        {a.artist} — {a.album}
                                    </Link>{" "}
                                    ({a.year ?? "—"}, {a.trackCount} tracks)
                                </td>
                                <td>{a.state}</td>
                                <td>
                                    {a.recommendation} {a.distance?.toFixed(3)}
                                </td>
                                <td>
                                    {a.issues.map((code) => (
                                        <IssueChip key={code} code={code} />
                                    ))}
                                </td>
                            </tr>
                        ))}
                    </tbody>
                </table>
                <p>{albums.data?.total ?? 0} albums</p>
                <Button
                    disabled={!page}
                    onClick={() => {
                        setPage(page - 1);
                        setSelected([]);
                    }}
                >
                    Previous albums
                </Button>
                <Button
                    disabled={
                        !albums.data || (page + 1) * 50 >= albums.data.total
                    }
                    onClick={() => {
                        setPage(page + 1);
                        setSelected([]);
                    }}
                >
                    Next albums
                </Button>
                <section>
                    <h2>Recent runs</h2>
                    {runs.data?.runs.map((r) => (
                        <Button
                            key={r.id}
                            aria-pressed={run === r.id}
                            onClick={() => setRun(r.id)}
                        >
                            {r.artist} — {r.album}: {r.state}
                        </Button>
                    ))}
                    {run && <RunPanel key={run} id={run} />}
                </section>
            </main>
        </div>
    );
}
