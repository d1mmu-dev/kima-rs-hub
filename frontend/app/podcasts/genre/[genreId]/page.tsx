"use client";

import { useState, useEffect, useRef, useCallback } from "react";
import { useParams, useRouter } from "next/navigation";
import { Mic2, ArrowLeft } from "lucide-react";
import { api } from "@/lib/api";
import { GradientSpinner } from "@/components/ui/GradientSpinner";
import { MediaCard } from "@/components/cards/MediaCard";
import { PageMessage } from "@/components/ui/PageMessage";
import { PODCAST_GENRES } from "@/features/podcast/genres";

interface Podcast {
    id: string;
    title: string;
    author: string;
    coverUrl: string;
    feedUrl: string;
    itunesId?: number;
}

export default function GenrePage() {
    const params = useParams();
    const router = useRouter();
    const genreId = params.genreId as string;
    const genre = PODCAST_GENRES.find((g) => g.id === genreId);

    const [podcasts, setPodcasts] = useState<Podcast[]>([]);
    const [loading, setLoading] = useState(false);
    const [hasMore, setHasMore] = useState(true);
    const observerRef = useRef<IntersectionObserver | null>(null);
    const loadMoreRef = useRef<HTMLDivElement>(null);
    const inFlightRef = useRef(false);
    const offsetRef = useRef(0);

    const LIMIT = 20;

    const loadMorePodcasts = useCallback(async () => {
        if (inFlightRef.current || !hasMore) return;
        inFlightRef.current = true;

        setLoading(true);
        try {
            const data = await api.getPodcastsByGenrePaginated(
                parseInt(genreId),
                LIMIT,
                offsetRef.current
            );

            if (data.length < LIMIT) {
                setHasMore(false);
            }

            offsetRef.current += data.length;
            setPodcasts((prev) => [...prev, ...data]);
        } catch (error) {
            console.error("Failed to load podcasts:", error);
            setHasMore(false);
        } finally {
            inFlightRef.current = false;
            setLoading(false);
        }
    }, [genreId, hasMore]);

    useEffect(() => {
        if (observerRef.current) observerRef.current.disconnect();

        observerRef.current = new IntersectionObserver(
            (entries) => {
                if (entries[0].isIntersecting && hasMore && !loading) {
                    loadMorePodcasts();
                }
            },
            { threshold: 0.1 }
        );

        if (loadMoreRef.current) {
            observerRef.current.observe(loadMoreRef.current);
        }

        return () => {
            if (observerRef.current) {
                observerRef.current.disconnect();
            }
        };
    }, [loadMorePodcasts, hasMore, loading]);

    useEffect(() => {
        loadMorePodcasts();
        // eslint-disable-next-line react-hooks/exhaustive-deps -- mount-only: initial load should not re-trigger when loadMorePodcasts identity changes
    }, []);

    if (!genre) {
        return (
            <PageMessage title="Genre not found" action={{ label: "Podcasts", href: "/podcasts" }} />
        );
    }

    return (
        <div className="min-h-screen relative bg-gradient-to-b from-[#0a0a0a] to-black">
            <div className="fixed inset-0 pointer-events-none opacity-50">
                <div className="absolute inset-0 bg-gradient-to-br from-white/5 via-transparent to-transparent" />
            </div>

            <div className="relative">
                <div className="relative bg-gradient-to-b from-[#0a0a0a] via-[#0f0f0f] to-transparent pt-6 pb-8 page-shell border-b border-white/5">
                    <div>
                        <button
                            onClick={() => router.push("/podcasts")}
                            className="flex items-center gap-2 text-xs tabular-nums text-[var(--text-muted)] hover:text-white transition-colors mb-6"
                        >
                            <ArrowLeft className="w-4 h-4" />
                            Back to Podcasts
                        </button>

                        <div className="flex items-center gap-2 mb-4">
                            <div className="w-1.5 h-1.5 bg-type-podcast rounded-full" />
                            <span className="t-eyebrow">
                                Genre
                            </span>
                        </div>

                        <h1 className="text-4xl md:text-5xl lg:text-6xl font-bold tracking-tight text-white leading-none mb-3">
                            {genre.name}
                        </h1>
                        <p className="text-sm tabular-nums text-[var(--text-muted)]">
                            {podcasts.length} podcast{podcasts.length !== 1 ? "s" : ""} found
                        </p>
                    </div>
                </div>

                <div className="relative page-shell pb-32 pt-8 animate-rise">
                    <div className="grid-media">
                        {podcasts.map((podcast, index) => (
                            <MediaCard
                                key={podcast.id}
                                href={`/podcasts/${podcast.id || podcast.itunesId}`}
                                title={podcast.title}
                                subtitle={podcast.author}
                                imageUrl={podcast.coverUrl}
                                fallbackIcon={Mic2}
                                type="podcast"
                                index={index}
                            />
                        ))}
                    </div>

                    {loading && (
                        <div className="flex justify-center items-center py-8">
                            <GradientSpinner size="md" />
                        </div>
                    )}

                    {/* Intersection observer target */}
                    <div ref={loadMoreRef} className="h-20" />

                    {!hasMore && podcasts.length > 0 && (
                        <div className="text-center py-8">
                            <span className="text-xs text-[var(--text-muted)]">
                                End of results
                            </span>
                        </div>
                    )}

                    {!loading && podcasts.length === 0 && (
                        <div className="text-center py-20">
                            <Mic2 className="w-16 h-16 text-[var(--text-muted)] mx-auto mb-4" />
                            <p className="text-xs text-[var(--text-muted)]">
                                No podcasts found
                            </p>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}
