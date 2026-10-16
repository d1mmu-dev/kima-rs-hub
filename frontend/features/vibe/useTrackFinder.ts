import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { searchLibrary, type LibraryIndex } from "./librarySearch";

export interface FoundTrack {
    id: string;
    title: string;
    artist: { name: string };
    /** How it was found: by its name, or by how it sounds. */
    via: "name" | "sound";
}

/**
 * Track lookup for the Song Path and Blend pickers: title or artist matches from
 * the loaded map first, then tracks whose sound matches the words from vibe search.
 */
export function useTrackFinder(library: LibraryIndex, query: string, enabled: boolean, limit = 10) {
    const q = query.trim();
    const active = enabled && q.length >= 2;

    const byName = useMemo<FoundTrack[]>(
        () => (active ? searchLibrary(library, q, limit).top.map((t) => ({ id: t.id, title: t.title, artist: { name: t.artist }, via: "name" as const })) : []),
        [active, library, q, limit],
    );

    const sound = useQuery({
        queryKey: ["vibe-picker-sound", q],
        queryFn: async () => (await api.vibeSearch(q, limit)).tracks,
        enabled: active,
        staleTime: 30_000,
    });

    const items = useMemo(() => {
        if (!active) return [];
        const seen = new Set(byName.map((t) => t.id));
        const bySound: FoundTrack[] = (sound.data ?? [])
            .filter((t) => !seen.has(t.id))
            .map((t) => ({ id: t.id, title: t.title, artist: { name: t.artist.name }, via: "sound" as const }));
        return [...byName, ...bySound].slice(0, limit);
    }, [active, byName, sound.data, limit]);

    return {
        items,
        searching: active && sound.isFetching && items.length === 0,
        empty: active && !sound.isFetching && items.length === 0,
    };
}
