"use client";

import DOMPurify from "dompurify";
import { Plus } from "lucide-react";
import { PodcastPreview } from "../types";
import { formatDuration } from "@/lib/format";
import { formatDate } from "../utils";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { Button } from "@/components/ui/Button";

interface PreviewEpisodesProps {
    previewData: PodcastPreview;
    isSubscribing: boolean;
    onSubscribe: () => void;
}

export function PreviewEpisodes({
    previewData,
    isSubscribing,
    onSubscribe,
}: PreviewEpisodesProps) {
    const hasEpisodes =
        previewData.previewEpisodes &&
        previewData.previewEpisodes.length > 0;

    return (
        <section>
            <SectionHeader title="Latest Episodes" type="podcast" />

            {hasEpisodes ? (
                // The subscribe fade below covers the episode list only, so the list holds it.
                <div className="relative">
                    <div className="space-y-0.5">
                        {previewData.previewEpisodes!.map((episode, index) => (
                            <div
                                key={index}
                                className="flex items-center gap-4 px-3 py-2 rounded-lg opacity-50 cursor-not-allowed"
                            >
                                <div className="w-8 flex items-center justify-center shrink-0">
                                    <span className="text-xs tabular-nums text-[var(--text-muted)]">
                                        {index + 1}
                                    </span>
                                </div>
                                <div className="flex-1 min-w-0">
                                    <h3 className="font-bold truncate text-sm text-white tracking-tight">
                                        {episode.title}
                                    </h3>
                                    <div className="flex items-center gap-2 text-xs tabular-nums text-[var(--text-muted)]">
                                        <span>{formatDate(episode.publishedAt)}</span>
                                        {episode.duration > 0 && (
                                            <>
                                                <span className="text-[var(--text-muted)]">|</span>
                                                <span>{formatDuration(episode.duration)}</span>
                                            </>
                                        )}
                                    </div>
                                </div>
                            </div>
                        ))}
                    </div>

                    <div className="absolute inset-0 bg-gradient-to-b from-transparent via-[#0a0a0a]/80 to-[#0a0a0a] flex items-end justify-center pb-8 pointer-events-none">
                        <Button
                            variant="primary"
                            onClick={onSubscribe}
                            isLoading={isSubscribing}
                            className="pointer-events-auto"
                        >
                            {isSubscribing ? "Subscribing" : (
                                <>
                                    <Plus className="w-4 h-4" />
                                    Subscribe to Unlock All Episodes
                                </>
                            )}
                        </Button>
                    </div>
                </div>
            ) : (
                <div className="relative overflow-hidden rounded-lg border border-white/10 bg-gradient-to-br from-[#0f0f0f] to-[#0a0a0a] p-8 text-center">
                    <p className="text-xs text-[var(--text-muted)] mb-4">
                        No episodes available for preview
                    </p>
                    <Button
                        variant="primary"
                        onClick={onSubscribe}
                        isLoading={isSubscribing}
                        className="mx-auto"
                    >
                        {isSubscribing ? "Subscribing" : (
                            <>
                                <Plus className="w-4 h-4" />
                                Subscribe
                            </>
                        )}
                    </Button>
                </div>
            )}

            {previewData.description && (
                <div className="mt-8">
                    <SectionHeader title="About" type="podcast" />
                    <div className="relative overflow-hidden rounded-lg border border-white/10 bg-[var(--bg-primary)] p-4">
                        <div
                            className="prose prose-invert prose-sm max-w-none text-[var(--text-secondary)] max-w-3xl text-xs leading-relaxed"
                            dangerouslySetInnerHTML={{
                                __html: DOMPurify.sanitize(previewData.description || ""),
                            }}
                        />
                    </div>
                </div>
            )}
        </section>
    );
}
