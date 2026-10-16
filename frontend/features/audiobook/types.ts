import type { AudiobookTrack } from "@/lib/audio-state-context";

interface AudiobookProgress {
    currentTime: number;
    progress: number;
    isFinished: boolean;
    lastPlayedAt: Date;
}

export interface AudiobookSection {
    index: number;
    title: string;
    start: number;
}

interface AudiobookSeries {
    name: string;
    sequence: string;
}

export interface Audiobook {
    id: string;
    title: string;
    author: string;
    narrator?: string;
    description?: string;
    coverUrl: string | null;
    duration: number;
    libraryId?: string;
    publisher?: string;
    publishedYear?: string;
    genres?: string[];
    series?: AudiobookSeries;
    isbn?: string;
    asin?: string;
    language?: string;
    progress?: AudiobookProgress | null;
    tracks?: AudiobookTrack[];
    trackCount?: number;
    sections?: AudiobookSection[];
}
