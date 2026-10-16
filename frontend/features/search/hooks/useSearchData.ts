import { useSearchQuery, useDiscoverSearchQuery, useDiscoverSimilarArtistsQuery } from "@/hooks/useQueries";
import type { SearchResult, DiscoverResult, AliasInfo } from "../types";
import { useMemo } from "react";

interface UseSearchDataProps {
    query: string;
}

interface UseSearchDataReturn {
    libraryResults: SearchResult | null;
    discoverResults: DiscoverResult[];
    similarArtists: DiscoverResult[];
    aliasInfo: AliasInfo | null;
    isLibrarySearching: boolean;
    isDiscoverSearching: boolean;
    hasSearched: boolean;
}

export function useSearchData({ query }: UseSearchDataProps): UseSearchDataReturn {
    const {
        data: libraryResults,
        isLoading: isLibrarySearching,
        isFetching: isLibraryFetching
    } = useSearchQuery(query, "all", 20);

    const {
        data: discoverData,
        isLoading: isDiscoverSearching,
        isFetching: isDiscoverFetching
    } = useDiscoverSearchQuery(query, "all", 20);

    const discoverResults = useMemo(() => {
        return discoverData?.results || [];
    }, [discoverData]);

    const aliasInfo = useMemo(() => {
        return discoverData?.aliasInfo || null;
    }, [discoverData]);

    const topArtist = useMemo(() => {
        const first = discoverResults.find((r) => r.type === "music");
        return first ? { name: first.name, mbid: first.mbid || "" } : null;
    }, [discoverResults]);

    // Separate query for similar artists -- fires after discover results load
    const { data: similarData } = useDiscoverSimilarArtistsQuery(
        topArtist?.name || "",
        topArtist?.mbid || ""
    );

    const similarArtists = useMemo(() => {
        return similarData?.similarArtists || [];
    }, [similarData]);

    const hasSearched = query.trim().length >= 2;

    return {
        libraryResults: libraryResults || null,
        discoverResults,
        similarArtists,
        aliasInfo,
        isLibrarySearching: isLibrarySearching || isLibraryFetching,
        isDiscoverSearching: isDiscoverSearching || isDiscoverFetching,
        hasSearched,
    };
}
