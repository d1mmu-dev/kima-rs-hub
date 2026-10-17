"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useAuth } from "@/lib/auth-context";
import { PageHeader } from "@/components/layout/PageHeader";
import { GradientSpinner } from "@/components/ui/GradientSpinner";
import { IssueChip } from "@/features/organizer/components/IssueChip";
import { NotAvailable } from "@/features/organizer/components/NotAvailable";
import { ISSUE_LABELS, issueLabel } from "@/features/organizer/filters";

export default function AttentionPage() {
    const { user } = useAuth();
    const isAdmin = user?.role === "admin";
    const [issue, setIssue] = useState("");

    const { data, isLoading, isError } = useQuery({
        queryKey: ["library", "attention"],
        queryFn: () => api.getAttention(),
        enabled: isAdmin,
    });

    const albums = useMemo(() => {
        const all = data ?? [];
        const kept = issue ? all.filter((a) => a.issues.includes(issue)) : all;
        return [...kept].sort((a, b) => a.artist.localeCompare(b.artist) || a.album.localeCompare(b.album));
    }, [data, issue]);

    // Albums grouped under their artist.
    const groups = useMemo(() => {
        const map = new Map<string, typeof albums>();
        for (const a of albums) map.set(a.artist, [...(map.get(a.artist) ?? []), a]);
        return [...map.entries()];
    }, [albums]);

    if (!isAdmin) {
        return <NotAvailable title="Admins only" description="Only an administrator can see this report." />;
    }

    return (
        <div className="min-h-screen pb-32">
            <PageHeader
                eyebrow="Settings"
                title="Needs attention"
                description="Albums where the scanner had to guess. This is a report; kima does not edit tags."
                actions={
                    <Link href="/settings/organize" className="text-sm text-[var(--text-secondary)] hover:text-white">
                        Organize library
                    </Link>
                }
            />
            <div className="page-shell section-stack">
                <div className="flex flex-wrap items-center gap-3 text-sm">
                    <label htmlFor="attention-issue" className="text-[var(--text-secondary)]">
                        Issue
                    </label>
                    <select
                        id="attention-issue"
                        value={issue}
                        onChange={(e) => setIssue(e.target.value)}
                        className="rounded border border-white/10 bg-[var(--bg-primary)] px-3 py-2 text-white"
                    >
                        <option value="">All issues</option>
                        {Object.keys(ISSUE_LABELS).map((code) => (
                            <option key={code} value={code}>
                                {issueLabel(code)}
                            </option>
                        ))}
                    </select>
                    <span className="text-xs text-[var(--text-muted)] tabular-nums">{albums.length} albums</span>
                </div>

                {isLoading && <GradientSpinner size="md" />}
                {isError && <p className="text-danger">Could not load the report.</p>}
                {data && albums.length === 0 && (
                    <p className="text-[var(--text-muted)]">
                        {issue ? "No albums have this issue." : "Nothing needs attention."}
                    </p>
                )}

                {groups.length > 0 && (
                    <div className="overflow-x-auto">
                        <table className="w-full text-left text-sm">
                            <thead className="text-xs text-[var(--text-muted)]">
                                <tr>
                                    <th className="py-2 pr-4 font-medium">Album</th>
                                    <th className="py-2 pr-4 font-medium">Year</th>
                                    <th className="py-2 font-medium">Issues</th>
                                </tr>
                            </thead>
                            {groups.map(([artist, list]) => (
                                <tbody key={artist}>
                                    <tr>
                                        <th colSpan={3} className="pt-5 pb-1 text-left text-white">
                                            {artist}
                                        </th>
                                    </tr>
                                    {list.map((a) => (
                                        <tr key={a.albumId} className="border-t border-white/5 align-top">
                                            <td className="py-2 pr-4 text-[var(--text-secondary)]">
                                                <Link href={`/album/?id=${encodeURIComponent(a.albumId)}`} className="hover:text-white">
                                                    {a.album}
                                                </Link>
                                            </td>
                                            <td className="py-2 pr-4 tabular-nums text-[var(--text-muted)]">{a.year ?? "-"}</td>
                                            <td className="py-2">
                                                <div className="flex flex-wrap gap-1">
                                                    {a.issues.map((c) => (
                                                        <IssueChip key={c} code={c} />
                                                    ))}
                                                </div>
                                            </td>
                                        </tr>
                                    ))}
                                </tbody>
                            ))}
                        </table>
                    </div>
                )}
            </div>
        </div>
    );
}
