"use client";

import { useRouter } from "next/router";
import { useInView } from "@/hooks/useInView";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { usePodcastChartQuery } from "@/hooks/useQueries";
import { PodcastShelf, PodcastShelfPlaceholder } from "./PodcastShelf";
import type { CategoryRef } from "../types";

const ROW = 12;

/** A chart category with richer inline controls. */
export function CategoryShelf({ category, enabled }: { category: CategoryRef; enabled: boolean }) {
    const { ref, shown: near } = useInView<HTMLElement>({
        threshold: 0,
        rootMargin: "600px 0px 600px 0px",
        withinMain: true,
    });
    const router = useRouter();

    // Queued for Apple behind anything a person is waiting on (a show page, the top shows).
    const { data: shows, isError, refetch, isFetching } = usePodcastChartQuery(
        category.id,
        ROW,
        enabled && near,
        true,
    );

    // A category whose chart Apple sends empty has nothing to show.
    if (shows && shows.length === 0) return null;

    const categories = [] as Array<{ name: string; id: string }>;
    if (enabled) {
        // In the new comparison layout Apple chart categories toggle an inline picker;
        // the category header still links to the official Apple page.
        categories.push({ name: category.name, id: category.id });
    }

    return (
        <section
            ref={ref}
            className="flex flex-col gap-4"
            data-tv-section={shows ? `category-${category.id}` : undefined}
        >
            <SectionHeader
                type="podcast"
                title={category.name}
                showAllHref={podcastCategoryHref(category.id)}
            />
            <CategoryChips
                categoryNames={categories.map((c) => c.name)}
                onSelect={(name) => {
                    const path = podcastCategoryHref(name);
                    router.push(path, { scroll: false }).catch(() => {});
                }}
            />
            {isError && !shows ? (
                <p className="text-sm text-[var(--text-muted)]">
                    couldn&apos;t reach Apple podcasts.{" "}
                    <button
                        type="button"
                        className="text-brand hover:underline disabled:opacity-50"
                        disabled={isFetching}
                        onClick={() => refetch()}
                    >
                        try again
                    </button>
                </p>
            ) : (
                <PodcastShelf placeholder={PodcastShelfPlaceholder} shows={shows || []} />
            )}
        </section>
    );
}
