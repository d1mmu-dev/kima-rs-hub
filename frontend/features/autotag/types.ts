export type Target = "files" | "copy" | "db";
export interface Capabilities {
    identifyEnabled: boolean;
    files: { available: boolean; reason: string | null };
    copy: {
        available: boolean;
        reason: string | null;
        copyRunId: string | null;
    };
    db: { available: boolean };
}
export interface Change {
    trackId: string | null;
    field: string;
    current: unknown;
    proposed: unknown;
}
export interface LocalTrack {
    id: string;
    path: string;
    disc: number;
    trackNo: number;
    title: string;
    artist: string;
    duration: number;
    recordingMbid: string | null;
}
export interface Candidate {
    id: string;
    releaseMbid: string;
    releaseGroupMbid: string;
    title: string;
    artist: string;
    date: string | null;
    country: string | null;
    media: (string | null)[];
    label: string | null;
    catalogNumber: string | null;
    barcode: string | null;
    trackCount: number;
    distance: number;
    recommendation: string;
    penalties: unknown;
    origin: string;
    fetchedAt: string | null;
    stale: boolean;
}
export interface Mapping {
    trackId: string;
    local: { disc: number; trackNo: number; title: string; duration: number };
    release: {
        medium: number;
        position: number;
        title: string;
        length: number | null;
        recordingMbid: string;
    };
    distance: number;
}
export interface Missing {
    medium: number;
    position: number;
    title: string;
    length?: number | null;
}
export interface Review {
    album: {
        id: string;
        artist: string;
        title: string;
        year: number | null;
        tracks: LocalTrack[];
    };
    state: string;
    recommendation: string | null;
    error: string | null;
    candidates: Candidate[];
    conflict: {
        albumId: string;
        artist: string;
        title: string;
        year: number | null;
    } | null;
    selected: {
        candidateId: string;
        changes: Change[];
        mapping: Mapping[];
        missing: Missing[];
        extra: string[];
    } | null;
    userMbidKept: boolean;
}
export interface Album {
    albumId: string;
    artist: string;
    album: string;
    year: number | null;
    trackCount: number;
    state: string;
    recommendation: string | null;
    distance: number | null;
    releaseMbid: string | null;
    searchedAt: string | null;
    appliedAt: string | null;
    issues: string[];
}
export interface Run {
    id: string;
    albumId: string;
    album: string;
    artist: string;
    target: Target;
    reorganize: boolean;
    state: string;
    createdAt: string | null;
    finishedAt: string | null;
    summary: Record<string, unknown> | null;
    error: string | null;
}
export interface RunDetail extends Run {
    organizerRunId?: string | null;
    rows: {
        id: number;
        trackId: string | null;
        path: string;
        state: string;
        note: string | null;
        changes: { field: string; old: unknown; new: unknown }[];
    }[];
    total: number;
}
export interface Search {
    albumIds?: string[];
    all?: boolean;
    query?: { artist: string; album: string };
    releaseMbid?: string;
}
export interface Apply {
    candidateId: string;
    target: Target;
    reorganize: boolean;
    mbidsOnly: boolean;
}
