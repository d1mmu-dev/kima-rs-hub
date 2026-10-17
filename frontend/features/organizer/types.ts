export type OrganizerMode = "copy" | "move";

export interface OrganizerCapabilities {
    copyAvailable: boolean;
    moveAvailable: boolean;
    libraryRoot: string | null;
    targetRoot: string | null;
}

export interface OrganizerRun {
    id: string;
    mode: OrganizerMode;
    /** planning | previewed | applying | done | failed | undoing | undone */
    state: string;
    sourceRoot: string;
    targetRoot: string;
    createdAt: string | null;
    finishedAt: string | null;
    switchedAt: string | null;
    summary: unknown;
    error: string | null;
}

export interface OrganizerRow {
    id: string;
    trackId: string | null;
    from: string;
    to: string;
    /** planned | unchanged | staged | done | skipped | failed | undo_staged | undo_failed | undone */
    state: string;
    note: string | null;
    collision: boolean;
    caseOnly?: boolean;
    albumId: string | null;
    album: string | null;
    artist: string | null;
    /** Attention codes the track raises by itself. */
    guessed: string[];
}

export interface OrganizerRunDetail extends OrganizerRun {
    rows: OrganizerRow[];
    total: number;
    offset: number;
    limit: number;
}

export interface AttentionAlbum {
    albumId: string;
    artist: string;
    album: string;
    year: number | null;
    issues: string[];
}

export interface LibraryProgress {
    done: number;
    total: number;
}
