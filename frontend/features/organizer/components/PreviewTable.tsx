"use client";

import { useMemo, useState } from "react";
import { filterRows, NO_FILTERS, type RowFilters } from "../filters";
import type { OrganizerRow } from "../types";
import { IssueChip } from "./IssueChip";

const FILTERS: Array<{ key: keyof RowFilters; label: string }> = [
    { key: "onlyChanges", label: "Only changes" },
    { key: "collisions", label: "Collisions" },
    { key: "guessed", label: "Guessed" },
];

/** Current path -> proposed path, with warnings, for one page of a run's journal. */
export function PreviewTable({ rows }: { rows: OrganizerRow[] }) {
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
                    {shown.length} of {rows.length} on this page
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
