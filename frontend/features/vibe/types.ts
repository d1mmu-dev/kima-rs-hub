export interface MapTrack {
    id: string;
    x: number;
    y: number;
    z: number;
    /** Place on the Kima logo's gradient, 0 the azure end to 1 the amber end; the colour of the track's area. */
    colour: number;
    title: string;
    artist: string;
    artistId: string;
    albumId: string;
    coverUrl: string | null;
    dominantMood: string;
    moods: Record<string, number>;
    energy: number | null;
    valence: number | null;
}

export type VibeMode = "idle" | "similar" | "search" | "path-picking" | "alchemy";
