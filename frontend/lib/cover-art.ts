import { api } from "@/lib/api";

/** The proxied cover-art URL for a cover id or an image URL; null when there is none. */
export function coverArtUrl(coverId: string | null | undefined, size = 300): string | null {
    return coverId ? api.getCoverArtUrl(coverId, size) : null;
}
