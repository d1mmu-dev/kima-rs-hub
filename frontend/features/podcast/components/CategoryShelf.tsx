"use client";

import { useInView } from "@/hooks/useInView";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { usePodcastChartQuery } from "@/hooks/useQueries";
import { podcastCategoryHref } from "../routes";
import { PodcastShelf, PodcastShelfPlaceholder } from "./PodcastShelf";
import type { CategoryRef } from "../types";

const ROW = 12;

/** A category's chart loads near the viewport, behind requests a person is waiting on. */
export function CategoryShelf({ category, enabled }: { category: CategoryRef; enabled: boolean }) {
    const { ref, shown: near } = useInView<HTMLElement>({
        threshold: 0,
        rootMargin: "600px 0px 600px 0px",
        withinMain: true,
    });
    const { data: shows, isError, refetch, isFetching } = usePodcastChartQuery(category.id, ROW, enabled && near, true);

    if (shows && shows.length === 0) return null;
    return (
        <section
            ref={ref}
            aria-busy={!shows && !isError ? true : undefined}
            data-tv-section={shows ? `category-${category.id}` : undefined}
        >
            <SectionHeader type="podcast" title={category.name} showAllHref={podcastCategoryHref(category.id)} />
            {isError && !shows ? (
                <p className="text-sm text-[var(--text-muted)]">
                    couldn&apos;t reach apple podcasts.{" "}
                    <button type="button" className="text-brand hover:underline disabled:opacity-50" disabled={isFetching} onClick={() => refetch()}>
                        try again
                    </button>
                </p>
            ) : shows ? (
                <PodcastShelf podcasts={shows} />
            ) : (
                <PodcastShelfPlaceholder />
            )}
        </section>
    );
}
