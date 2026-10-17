import type { OrganizerRow } from "./types";

export interface RowFilters {
    onlyChanges: boolean;
    collisions: boolean;
    guessed: boolean;
}

export const NO_FILTERS: RowFilters = { onlyChanges: false, collisions: false, guessed: false };

/** Filters are cumulative: a row must pass every one that is on. */
export function filterRows(rows: OrganizerRow[], f: RowFilters): OrganizerRow[] {
    return rows.filter((r) => {
        if (f.onlyChanges && (r.state === "unchanged" || r.from === r.to)) return false;
        if (f.collisions && !r.collision) return false;
        if (f.guessed && r.guessed.length === 0) return false;
        return true;
    });
}

export const ISSUE_LABELS: Record<string, string> = {
    artistFromPath: "Artist from folder",
    albumFromPath: "Album from folder",
    unknownArtist: "Unknown artist",
    unknownAlbum: "Unknown album",
    missingYear: "No year",
    conflictingYears: "Conflicting years",
    missingTrackNumbers: "Missing track numbers",
    duplicateTrackNumbers: "Duplicate track numbers",
    folderSplit: "Folder split across albums",
};

export function issueLabel(code: string): string {
    return ISSUE_LABELS[code] ?? code;
}
