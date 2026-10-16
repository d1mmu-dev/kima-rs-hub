"use client";

import { Suspense } from "react";

import DOMPurify from "dompurify";
import { LoadingScreen } from "@/components/ui/LoadingScreen";
import { useImageColor } from "@/hooks/useImageColor";
import { api } from "@/lib/api";
import { notify } from "@/lib/notify";
import type { Episode } from "@/features/podcast/types";

import { usePodcastData } from "@/features/podcast/hooks/usePodcastData";
import { usePodcastActions } from "@/features/podcast/hooks/usePodcastActions";

import { PodcastHero } from "@/features/podcast/components/PodcastHero";
import { ColorWash } from "@/features/spoken/ColorWash";
import { PodcastActionBar } from "@/features/podcast/components/PodcastActionBar";
import { ContinueListening } from "@/features/podcast/components/ContinueListening";
import { EpisodeList } from "@/features/podcast/components/EpisodeList";
import { PreviewEpisodes } from "@/features/podcast/components/PreviewEpisodes";
import { SimilarPodcasts } from "@/features/podcast/components/SimilarPodcasts";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { PageMessage } from "@/components/ui/PageMessage";

function PodcastDetailPageInner() {
    const {
        podcastId,
        podcast,
        previewData,
        previewLoadState,
        previewError,
        displayData,
        isLoading,
        heroImage,
        colorExtractionImage,
        similarPodcasts,
        sortOrder,
        setSortOrder,
        inProgressEpisodes,
        sortedEpisodes,
        episodesUpdating,
        hasMore,
        loadMore,
        loadingMore,
    } = usePodcastData();

    const { colors } = useImageColor(colorExtractionImage);

    const {
        isSubscribing,
        isRefreshing,
        showDeleteConfirm,
        setShowDeleteConfirm,
        handleSubscribe,
        handleRemovePodcast,
        handleRefresh,
        handlePlayEpisode,
        handlePlayPauseEpisode,
        handleMarkEpisodeComplete,
        isEpisodePlaying,
        isPlaying,
        pause,
    } = usePodcastActions(podcastId, sortedEpisodes);

    if (previewLoadState === "error") {
        return (
            <PageMessage
                title="Couldn't load this podcast"
                description={previewError || undefined}
                action={{ label: "Podcasts", href: "/podcasts" }}
            />
        );
    }

    if (isLoading) {
        return <LoadingScreen />;
    }

    if (!podcast && !previewData) {
        return (
            <PageMessage title="Podcast not found" action={{ label: "Podcasts", href: "/podcasts" }} />
        );
    }

    if (!displayData) {
        return <LoadingScreen />;
    }

    const isSubscribed = !!podcast;
    const episodeCount = podcast
        ? podcast.episodeTotal ?? podcast.episodes.length
        : previewData?.episodeCount || 0;

    // The most recent unfinished episode, or the latest; the list shown oldest
    // first holds the oldest page, so the newest is asked for.
    const handlePlayLatest = async () => {
        if (!podcast) return;
        try {
            const newest: Episode[] =
                sortOrder === "newest" ? sortedEpisodes : (await api.getPodcast(podcast.id, { order: "newest", limit: 20 })).episodes;
            const pick = newest.find((ep) => !ep.progress?.isFinished) ?? newest[0];
            if (pick) handlePlayEpisode(pick, podcast, newest);
        } catch (error) {
            notify.error("Couldn't load the latest episode", { message: error instanceof Error ? error.message : undefined });
        }
    };

    // Check if any episode from this podcast is currently playing
    const isPlayingPodcast = sortedEpisodes.some((ep) => isEpisodePlaying(ep.id)) && isPlaying;

    return (
        <div className="min-h-screen flex flex-col bg-gradient-to-b from-[#0a0a0a] to-black">
            <PodcastHero
                title={displayData.title}
                author={displayData.author}
                description={displayData.description}
                genres={displayData.genres}
                heroImage={heroImage}
                colors={colors}
                episodeCount={episodeCount}
                inProgressCount={inProgressEpisodes.length}
            >
                <PodcastActionBar
                    isSubscribed={isSubscribed}
                    feedUrl={podcast?.feedUrl || previewData?.feedUrl}
                    isSubscribing={isSubscribing}
                    showDeleteConfirm={showDeleteConfirm}
                    onSubscribe={() => handleSubscribe(previewData)}
                    onRemove={handleRemovePodcast}
                    onShowDeleteConfirm={setShowDeleteConfirm}
                    onPlayLatest={isSubscribed ? handlePlayLatest : undefined}
                    isPlayingPodcast={isPlayingPodcast}
                    onPause={pause}
                    onRefresh={handleRefresh}
                    isRefreshing={isRefreshing}
                />
            </PodcastHero>

            <div className="relative flex-1">
                <ColorWash colors={colors} />

                <div className="relative page-shell py-8 section-stack">
                    {podcast && inProgressEpisodes.length > 0 && (
                        <div>
                            <ContinueListening
                                inProgressEpisodes={inProgressEpisodes}
                                sortedEpisodes={sortedEpisodes}
                                isEpisodePlaying={isEpisodePlaying}
                                isPlaying={isPlaying}
                                onPlayEpisode={(episode) => handlePlayEpisode(episode, podcast)}
                                onPlayPause={(episode) =>
                                    handlePlayPauseEpisode(episode, podcast)
                                }
                            />
                        </div>
                    )}

                    {!podcast && previewData && (
                        <div>
                            <PreviewEpisodes
                                previewData={previewData}
                                isSubscribing={isSubscribing}
                                onSubscribe={() => handleSubscribe(previewData)}
                            />
                        </div>
                    )}

                    {podcast && (
                        <div>
                            <EpisodeList
                                episodes={sortedEpisodes}
                                sortOrder={sortOrder}
                                onSortOrderChange={setSortOrder}
                                isEpisodePlaying={isEpisodePlaying}
                                isPlaying={isPlaying}
                                onPlayPause={(episode) =>
                                    handlePlayPauseEpisode(episode, podcast)
                                }
                                onMarkComplete={handleMarkEpisodeComplete}
                                fallbackCoverUrl={podcast?.coverUrl}
                                total={episodeCount}
                                updating={episodesUpdating}
                                onLoadMore={hasMore ? () => loadMore() : undefined}
                                loadingMore={loadingMore}
                            />
                        </div>
                    )}

                    {podcast?.description && (
                        <div>
                            <SectionHeader title="About" type="podcast" />
                            <div className="relative overflow-hidden rounded-lg border border-white/10 bg-[var(--bg-primary)] p-5">
                                <div
                                    className="prose prose-invert prose-sm max-w-none text-[var(--text-secondary)] max-w-3xl text-sm leading-relaxed"
                                    dangerouslySetInnerHTML={{
                                        __html: DOMPurify.sanitize(podcast.description),
                                    }}
                                />
                            </div>
                        </div>
                    )}

                    {similarPodcasts.length > 0 && (
                        <div>
                            <SimilarPodcasts podcasts={similarPodcasts} />
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}

export default function PodcastDetailPage() {
    return (
        <Suspense fallback={null}>
            <PodcastDetailPageInner />
        </Suspense>
    );
}
