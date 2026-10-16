"use client";

import { useEffect, useState, useRef, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { useRouter } from "next/navigation";
import { useAuth } from "@/lib/auth-context";
import { notify } from "@/lib/notify";
import { Mic2, Search, Plus, RefreshCw, Rss, X } from "lucide-react";
import { GradientSpinner } from "@/components/ui/GradientSpinner";
import { LoadingScreen } from "@/components/ui/LoadingScreen";
import { Pagination } from "@/components/ui/Pagination";
import { usePodcastsQuery, useTopPodcastsQuery, queryKeys } from "@/hooks/useQueries";
import { cn } from "@/lib/cn";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { PageHeader } from "@/components/layout/PageHeader";
import { Button } from "@/components/ui/Button";
import { CachedImage } from "@/components/ui/CachedImage";
import { MediaCard } from "@/components/cards/MediaCard";
import { scrollMainToTop } from "@/lib/scroll";
import { PODCAST_GENRES } from "@/features/podcast/genres";
import { coverArtUrl } from "@/lib/cover-art";

function PodcastCard({ podcast, index }: { podcast: { id: string; title: string; author: string; coverUrl?: string }; index: number }) {
    return (
        <MediaCard
            href={`/podcasts/${podcast.id}`}
            title={podcast.title}
            subtitle={podcast.author}
            imageUrl={coverArtUrl(podcast.coverUrl)}
            fallbackIcon={Mic2}
            type="podcast"
            index={index}
        />
    );
}

function PodcastGrid({
    podcasts,
    section,
}: {
    podcasts: { id: string; title: string; author: string; coverUrl?: string }[];
    section: string;
}) {
    return (
        <div className="grid-media" data-tv-section={section}>
            {podcasts.map((podcast, index) => (
                <PodcastCard key={podcast.id} podcast={podcast} index={index} />
            ))}
        </div>
    );
}

interface SearchResult {
    type?: string;
    id: number | string;
    name?: string;
    artist?: string;
    title?: string;
    author?: string;
    coverUrl: string;
    feedUrl: string | null;
    trackCount?: number;
    itunesId?: number | null;
}

export default function PodcastsPage() {
    const [searchQuery, setSearchQuery] = useState("");
    const [searchResults, setSearchResults] = useState<SearchResult[]>([]);
    const [isSearching, setIsSearching] = useState(false);
    const [showDropdown, setShowDropdown] = useState(false);
    const searchTimeoutRef = useRef<NodeJS.Timeout | null>(null);
    const dropdownRef = useRef<HTMLDivElement>(null);
    const { isAuthenticated } = useAuth();
    const router = useRouter();
    const queryClient = useQueryClient();

    const [showRssInput, setShowRssInput] = useState(false);
    const [rssUrl, setRssUrl] = useState("");
    const [isSubscribingRss, setIsSubscribingRss] = useState(false);
    const [rssError, setRssError] = useState<string | null>(null);

    const { data: podcasts = [], isLoading: isLoadingPodcasts } =
        usePodcastsQuery();
    const { data: topPodcasts = [], isLoading: isLoadingTopPodcasts } =
        useTopPodcastsQuery(12);

    const { data: relatedPodcasts = {} } = useQuery({
        queryKey: ["podcasts", "discovery", "genres"],
        queryFn: async () => {
            return api.getPodcastsByGenre(PODCAST_GENRES.map((genre) => Number(genre.id)));
        },
        staleTime: 10 * 60 * 1000,
        enabled: isAuthenticated,
    });

    type SortOption = "title" | "author" | "recent";
    const [sortBy, setSortBy] = useState<SortOption>("title");
    const [itemsPerPage, setItemsPerPage] = useState<number>(50);
    const [currentPage, setCurrentPage] = useState(1);
    const showPage = (page: number) => {
        setCurrentPage(page);
        scrollMainToTop();
    };

    const isLoading = isLoadingPodcasts || isLoadingTopPodcasts;
    const [isRefreshingAll, setIsRefreshingAll] = useState(false);

    const handleRefreshAll = async () => {
        setIsRefreshingAll(true);
        try {
            await api.refreshAllPodcasts();
            queryClient.invalidateQueries({ queryKey: queryKeys.podcasts() });
        } catch (error) {
            console.error("Failed to refresh podcasts:", error);
            notify.error("Failed to refresh podcasts", { message: error instanceof Error ? error.message : undefined });
        } finally {
            setIsRefreshingAll(false);
        }
    };

    const handleRssSubscribe = async () => {
        const url = rssUrl.trim();
        if (!url) return;

        try {
            new URL(url);
        } catch {
            setRssError("Please enter a valid URL");
            return;
        }

        setIsSubscribingRss(true);
        setRssError(null);
        try {
            const result = await api.subscribePodcast(url);
            if (result.success && result.podcast?.id) {
                queryClient.invalidateQueries({ queryKey: queryKeys.podcasts() });
                router.push(`/podcasts/${result.podcast.id}`);
            }
            setRssUrl("");
            setShowRssInput(false);
        } catch (error: unknown) {
            setRssError(error instanceof Error ? error.message : "Failed to subscribe");
        } finally {
            setIsSubscribingRss(false);
        }
    };

    const sortedPodcasts = useMemo(() => {
        const sorted = [...podcasts];
        switch (sortBy) {
            case "title":
                sorted.sort((a, b) => (a.title || "").localeCompare(b.title || ""));
                break;
            case "author":
                sorted.sort((a, b) => (a.author || "").localeCompare(b.author || ""));
                break;
            case "recent":
                sorted.sort((a, b) => (b.episodeCount || 0) - (a.episodeCount || 0));
                break;
        }
        return sorted;
    }, [podcasts, sortBy]);

    const totalPages = Math.ceil(sortedPodcasts.length / itemsPerPage);
    const paginatedPodcasts = useMemo(() => {
        const start = (currentPage - 1) * itemsPerPage;
        return sortedPodcasts.slice(start, start + itemsPerPage);
    }, [sortedPodcasts, currentPage, itemsPerPage]);

    useEffect(() => {
        setCurrentPage(1);
    }, [sortBy]);

    useEffect(() => {
        const handleClickOutside = (event: MouseEvent) => {
            if (
                dropdownRef.current &&
                !dropdownRef.current.contains(event.target as Node)
            ) {
                setShowDropdown(false);
            }
        };
        document.addEventListener("mousedown", handleClickOutside);
        return () => document.removeEventListener("mousedown", handleClickOutside);
    }, []);

    useEffect(() => {
        if (searchTimeoutRef.current) {
            clearTimeout(searchTimeoutRef.current);
        }

        if (searchQuery.trim().length < 2) {
            setSearchResults([]);
            setShowDropdown(false);
            return;
        }

        setIsSearching(true);
        searchTimeoutRef.current = setTimeout(async () => {
            try {
                const results = await api.discoverSearch(searchQuery, "podcasts", 8);
                const podcastResults =
                    results?.results?.filter(
                        (r: { type: string }) => r.type === "podcast"
                    ) || [];
                setSearchResults(podcastResults);
                setShowDropdown(podcastResults.length > 0);
            } catch (error) {
                console.error("Podcast search failed:", error);
                setSearchResults([]);
                setShowDropdown(false);
            } finally {
                setIsSearching(false);
            }
        }, 500);

        return () => {
            if (searchTimeoutRef.current) {
                clearTimeout(searchTimeoutRef.current);
            }
        };
    }, [searchQuery]);

    if (isLoading) {
        return <LoadingScreen />;
    }

    return (
        <div className="min-h-screen">
            <PageHeader
                eyebrow="Your library"
                title="Podcasts"
                description={podcasts.length > 0 ? `${podcasts.length} subscribed` : "Subscribe to shows and listen to them here"}
                actions={
                    <>
                        {!showRssInput && (
                            <Button variant="secondary" onClick={() => setShowRssInput(true)}>
                                <Rss className="w-4 h-4" />
                                Add RSS feed
                            </Button>
                        )}
                        <div className="relative w-full sm:w-80" ref={dropdownRef}>
                            <Search className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-[var(--text-muted)] z-(--z-raised)" />
                            <input
                                type="text"
                                value={searchQuery}
                                onChange={(e) => setSearchQuery(e.target.value)}
                                placeholder="Search podcasts to add"
                                role="combobox"
                                aria-expanded={showDropdown}
                                aria-controls="podcast-search-listbox"
                                aria-label="Search podcasts to subscribe"
                                aria-autocomplete="list"
                                className="w-full pl-10 pr-4 py-2.5 bg-[var(--bg-primary)] border-2 border-white/10 rounded-lg text-white placeholder-gray-600 focus:outline-none focus:border-brand/50 transition-all text-sm tabular-nums"
                            />
                            {isSearching && (
                                <div className="absolute right-3 top-1/2 -translate-y-1/2 z-(--z-raised)">
                                    <GradientSpinner size="sm" />
                                </div>
                            )}

                            {showDropdown && searchResults.length > 0 && (
                                <div
                                    id="podcast-search-listbox"
                                    role="listbox"
                                    aria-label="Podcast search results"
                                    className="absolute top-full left-0 mt-2 w-full bg-[var(--bg-secondary)] border-2 border-white/10 rounded-lg overflow-hidden z-(--z-dropdown) max-h-96 overflow-y-auto"
                                >
                                    {searchResults.map((result) => {
                                        const imageUrl = coverArtUrl(result.coverUrl);
                                        return (
                                            <div
                                                key={result.id}
                                                role="option"
                                                aria-selected={false}
                                                className="flex items-center gap-3 p-3 hover:bg-white/5 transition-colors cursor-pointer border-b border-white/5 last:border-b-0"
                                                onClick={() => {
                                                    router.push(`/podcasts/${result.id}`);
                                                    setShowDropdown(false);
                                                }}
                                            >
                                                <div className="w-10 h-10 rounded-lg bg-[var(--bg-primary)] flex-shrink-0 overflow-hidden relative border border-white/10">
                                                    {imageUrl ? (
                                                        <CachedImage
                                                            src={imageUrl}
                                                            alt={result.name || "Podcast"}
                                                            fill
                                                            sizes="40px"
                                                            className="object-cover"
                                                        />
                                                    ) : (
                                                        <div className="w-full h-full flex items-center justify-center">
                                                            <Mic2 className="w-4 h-4 text-[var(--text-muted)]" />
                                                        </div>
                                                    )}
                                                </div>
                                                <div className="flex-1 min-w-0">
                                                    <h3 className="text-sm font-bold text-white truncate tracking-tight">
                                                        {result.name}
                                                    </h3>
                                                    <p className="text-xs tabular-nums text-[var(--text-muted)] truncate">
                                                        {result.artist}
                                                    </p>
                                                </div>
                                                <div className="flex-shrink-0">
                                                    <button
                                                        onClick={(e) => {
                                                            e.stopPropagation();
                                                            router.push(`/podcasts/${result.id}`);
                                                            setShowDropdown(false);
                                                        }}
                                                        aria-label={`Subscribe to ${result.name}`}
                                                        className="min-h-[44px] min-w-[44px] rounded-lg bg-brand hover:bg-brand-hover flex items-center justify-center transition-colors"
                                                    >
                                                        <Plus className="w-3.5 h-3.5 text-white" />
                                                    </button>
                                                </div>
                                            </div>
                                        );
                                    })}
                                </div>
                            )}

                            {showDropdown &&
                                searchResults.length === 0 &&
                                !isSearching &&
                                searchQuery.length >= 2 && (
                                    <div className="absolute top-full left-0 mt-2 w-full bg-[var(--bg-secondary)] border-2 border-white/10 rounded-lg p-4 z-(--z-dropdown)">
                                        <p className="text-xs text-[var(--text-muted)] text-center">
                                            No podcasts found
                                        </p>
                                    </div>
                                )}
                        </div>
                    </>
                }
            >
                {showRssInput && (
                    <div className="mt-4 flex gap-2 items-start max-w-xl">
                        <div className="flex-1">
                            <div className="flex gap-2">
                                <input
                                    type="url"
                                    value={rssUrl}
                                    onChange={(e) => { setRssUrl(e.target.value); setRssError(null); }}
                                    onKeyDown={(e) => e.key === "Enter" && handleRssSubscribe()}
                                    placeholder="https://example.com/feed.xml"
                                    aria-label="RSS feed URL"
                                    className="flex-1 px-4 py-2.5 bg-white/5 border border-white/10 rounded-lg text-white placeholder-gray-500 focus:outline-none focus:border-brand text-sm"
                                    autoFocus
                                />
                                <Button
                                    variant="primary"
                                    onClick={handleRssSubscribe}
                                    disabled={!rssUrl.trim()}
                                    isLoading={isSubscribingRss}
                                    className="whitespace-nowrap"
                                >
                                    {!isSubscribingRss && <Plus className="w-4 h-4" />}
                                    Subscribe
                                </Button>
                                <button
                                    onClick={() => { setShowRssInput(false); setRssUrl(""); setRssError(null); }}
                                    aria-label="Cancel"
                                    className="p-2.5 rounded-lg text-[var(--text-muted)] hover:text-white hover:bg-white/5 transition-all"
                                >
                                    <X className="w-4 h-4" />
                                </button>
                            </div>
                            {rssError && <p className="text-danger-light text-xs mt-1">{rssError}</p>}
                        </div>
                    </div>
                )}
            </PageHeader>

            <div className="relative page-shell pb-32 pt-2 animate-rise">
                <div className="section-stack">
                    {podcasts.length > 0 && (
                        <section>
                            <SectionHeader type="podcast"
                                title="My Podcasts"
                                count={podcasts.length}
                                action={
                                    <div className="flex items-center gap-2">
                                        <button
                                            onClick={handleRefreshAll}
                                            disabled={isRefreshingAll}
                                            className="flex items-center gap-2 px-3 py-1.5 rounded-lg text-[var(--text-muted)] hover:text-white hover:bg-white/5 border border-transparent hover:border-white/10 transition-all text-xs disabled:opacity-50"
                                            title="check all podcasts for new episodes"
                                        >
                                            <RefreshCw className={cn("w-3.5 h-3.5", isRefreshingAll && "animate-spin")} />
                                            <span className="hidden md:inline">{isRefreshingAll ? "Refreshing..." : "Refresh All"}</span>
                                        </button>
                                        <select
                                            value={sortBy}
                                            onChange={(e) => setSortBy(e.target.value as SortOption)}
                                            aria-label="Sort podcasts"
                                            className="px-3 py-1.5 bg-[var(--bg-primary)] border-2 border-white/10 rounded-lg text-white text-xs focus:outline-none focus:border-brand/50 [&>option]:bg-[var(--bg-primary)] [&>option]:text-white cursor-pointer"
                                        >
                                            <option value="title">title</option>
                                            <option value="author">author</option>
                                            <option value="recent">episodes</option>
                                        </select>
                                        <select
                                            value={itemsPerPage}
                                            onChange={(e) => {
                                                setItemsPerPage(Number(e.target.value));
                                                setCurrentPage(1);
                                            }}
                                            aria-label="Podcasts per page"
                                            className="px-3 py-1.5 bg-[var(--bg-primary)] border-2 border-white/10 rounded-lg text-white text-xs tabular-nums focus:outline-none focus:border-brand/50 [&>option]:bg-[var(--bg-primary)] [&>option]:text-white cursor-pointer"
                                        >
                                            <option value={25}>25</option>
                                            <option value={50}>50</option>
                                            <option value={100}>100</option>
                                            <option value={250}>250</option>
                                        </select>
                                    </div>
                                }
                            />
                            <PodcastGrid podcasts={paginatedPodcasts} section="my-podcasts" />

                            <Pagination
                                currentPage={currentPage}
                                totalPages={totalPages}
                                onPageChange={showPage}
                                label="My podcasts"
                                className="mt-8"
                            />
                        </section>
                    )}

                    {topPodcasts.length > 0 && (
                        <section>
                            <SectionHeader type="podcast" title="Top Podcasts" />
                            <PodcastGrid podcasts={topPodcasts} section="top-podcasts" />
                        </section>
                    )}

                    {PODCAST_GENRES.map(({ id: genreId, name: genreName }) => {
                        const genrePodcasts = relatedPodcasts[genreId] || [];
                        return genrePodcasts.length > 0 ? (
                            <section key={genreId}>
                                <SectionHeader type="podcast"
                                    title={genreName}
                                    action={
                                        <button
                                            onClick={() => router.push(`/podcasts/genre/${genreId}`)}
                                            className="text-xs text-[var(--text-muted)] hover:text-brand transition-colors"
                                        >
                                            View All
                                        </button>
                                    }
                                />
                                <PodcastGrid podcasts={genrePodcasts} section={`genre-${genreId}`} />
                            </section>
                        ) : null;
                    })}

                    {podcasts.length === 0 && topPodcasts.length === 0 && (
                        <section>
                            <div className="relative overflow-hidden rounded-lg border-2 border-white/10 bg-gradient-to-br from-[#0f0f0f] to-[#0a0a0a] p-12">
                                <div className="absolute top-0 left-0 w-1 h-full bg-type-podcast" />
                                <div className="flex flex-col items-center text-center">
                                    <Mic2 className="w-16 h-16 text-[var(--text-muted)] mb-6" />
                                    <h2 className="text-2xl font-bold tracking-tight text-white mb-2">
                                        Discover Podcasts
                                    </h2>
                                    <p className="text-sm text-[var(--text-muted)] max-w-md">
                                        Search for podcasts above to subscribe and start listening
                                    </p>
                                </div>
                            </div>
                        </section>
                    )}
                </div>
            </div>
        </div>
    );
}
