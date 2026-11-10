// Static export: dynamic show and category paths cannot be built for unknown IDs.
export const podcastShowHref = (id: string | number) => `/podcasts/show/?id=${encodeURIComponent(String(id))}`;
export const podcastCategoryHref = (id: string | number) => `/podcasts/category/?id=${encodeURIComponent(String(id))}`;

export function parsePodcastCategoryId(id: string | null): number | null | undefined {
    if (id === "top") return null;
    if (!id || !/^[1-9]\d*$/.test(id)) return undefined;
    const category = Number(id);
    return Number.isSafeInteger(category) ? category : undefined;
}
