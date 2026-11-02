"use client";

import { useMemo, useState } from "react";
import { Button } from "@/components/ui/Button";
import { filterRows, NO_FILTERS, type RowFilters } from "../filters";
import type { OrganizerAlbumGroup, OrganizerRow } from "../types";
import { IssueChip } from "./IssueChip";

export const ALBUMS_PER_PAGE = 20;

/** Zero-based album pages; every group's journal rows stay together. */
export function AlbumGroupedPreview({
    groups,
    page,
    total,
    onPage,
}: {
    groups: OrganizerAlbumGroup[];
    page: number;
    total: number;
    onPage: (n: number) => void;
}) {
    return (
        <div className="space-y-4">
            {groups.map((group) => (
                <details key={group.albumId ?? "other"} open className="rounded border border-white/10 p-3">
                    <summary className="cursor-pointer text-sm text-white">
                        {group.albumId === null
                            ? "Other files"
                            : `${group.artist ?? "Unknown artist"} — ${group.album ?? "Unknown album"}`}
                        <span className="mx-3 text-[var(--text-muted)]">{group.rows.length} files</span>
                        {[...new Set(group.rows.flatMap((row) => row.guessed))].map((code) => (
                            <IssueChip key={code} code={code} />
                        ))}
                    </summary>
                    <div className="mt-3">
                        <GroupRows rows={group.rows} />
                    </div>
                </details>
            ))}
            {total > ALBUMS_PER_PAGE && (
                <nav
                    aria-label="Album preview pages"
                    className="flex items-center gap-3 text-sm text-[var(--text-secondary)]"
                >
                    <Button variant="ghost" size="sm" disabled={page === 0} onClick={() => onPage(page - 1)}>
                        Previous
                    </Button>
                    <span>
                        {page * ALBUMS_PER_PAGE + 1}-{Math.min((page + 1) * ALBUMS_PER_PAGE, total)} of {total} albums
                    </span>
                    <Button
                        variant="ghost"
                        size="sm"
                        disabled={(page + 1) * ALBUMS_PER_PAGE >= total}
                        onClick={() => onPage(page + 1)}
                    >
                        Next
                    </Button>
                </nav>
            )}
        </div>
    );
}

const FILTERS: Array<{ key: keyof RowFilters; label: string }> = [
    { key: "onlyChanges", label: "Only changes" },
    { key: "collisions", label: "Collisions" },
    { key: "guessed", label: "Guessed" },
];

/** Independent, cumulative filters for the complete journal of one album. */
function GroupRows({ rows }: { rows: OrganizerRow[] }) {
    const [filters, setFilters] = useState<RowFilters>(NO_FILTERS);
    const shown = useMemo(() => filterRows(rows, filters), [rows, filters]);

    return (
        <div>
            <div className="mb-3 flex flex-wrap items-center gap-4 text-sm">
                {FILTERS.map((f) => (
                    <label key={f.key} className="flex items-center gap-2 text-[var(--text-secondary)]">
                        <input
                            type="checkbox"
                            checked={filters[f.key]}
                            onChange={(e) => setFilters((prev) => ({ ...prev, [f.key]: e.target.checked }))}
                        />
                        {f.label}
                    </label>
                ))}
                <span className="text-xs text-[var(--text-muted)] tabular-nums">
                    {shown.length} of {rows.length} in this group
                </span>
            </div>
            <div className="overflow-x-auto">
                <table className="w-full text-left text-xs">
                    <thead className="text-[var(--text-muted)]">
                        <tr>
                            <th className="py-2 pr-4 font-medium">From</th>
                            <th className="py-2 pr-4 font-medium">To</th>
                            <th className="py-2 font-medium">Warnings</th>
                        </tr>
                    </thead>
                    <tbody>
                        {shown.map((r) => (
                            <tr key={r.id} className="border-t border-white/5 align-top">
                                <td className="max-w-md break-all py-2 pr-4 text-[var(--text-muted)]">{r.from}</td>
                                <td className="max-w-md break-all py-2 pr-4 text-white">
                                    {r.state === "unchanged" ? (
                                        <span className="text-[var(--text-muted)]">unchanged</span>
                                    ) : (
                                        r.to
                                    )}
                                </td>
                                <td className="py-2">
                                    <div className="flex flex-wrap gap-1">
                                        {r.collision && (
                                            <span className="rounded border border-danger/40 px-2 py-0.5 text-danger">
                                                Collision
                                            </span>
                                        )}
                                        {r.state !== "planned" && r.state !== "unchanged" && (
                                            <span className="rounded border border-white/10 px-2 py-0.5 text-[var(--text-secondary)]">
                                                {r.state.replace("_", " ")}
                                            </span>
                                        )}
                                        {r.guessed.map((c) => (
                                            <IssueChip key={c} code={c} />
                                        ))}
                                        {r.note && <span className="text-[var(--text-muted)]">{r.note}</span>}
                                    </div>
                                </td>
                            </tr>
                        ))}
                        {shown.length === 0 && (
                            <tr>
                                <td colSpan={3} className="py-6 text-center text-[var(--text-muted)]">
                                    Nothing matches these filters.
                                </td>
                            </tr>
                        )}
                    </tbody>
                </table>
            </div>
        </div>
    );
}
