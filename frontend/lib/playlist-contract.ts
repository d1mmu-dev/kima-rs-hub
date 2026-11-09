/**
 * Named payload contract for playlist item API calls.
 *
 * Mirrors the backend DTOs that back the `POST /playlists/:id/items` route family
 * and the per-item `POST /playlists/:id/items/:itemId/move` route, so the client
 * and server agree on the wire shape before a request is sent.
 */

/** Payload for adding one or more tracks to a playlist. */
export interface AddPlaylistItemContractingBody {
    /**
     * Single track to add.
     * Exactly one of `trackId` / `trackIds` must be present.
     */
    trackId?: string;

    /**
     * Multiple tracks to add in one request.
     * Exactly one of `trackId` / `trackIds` must be present.
     */
    trackIds?: string[];
}

/** Payload for moving a playlist item. */
export interface MovePlaylistItemContractingBody {
    /**
     * The item to move after. `null` (or omitted) appends the item to the end.
     */
    afterItemId?: string | null;
}

/**
 * Builds the request body for `POST /playlists/:id/itemsContracting`.
 *
 * Pass only `trackId` for a single item or only `trackIds` for a batch. The
 * backend rejects a body carrying both, so this helper enforces that split.
 */
export function createAddPlaylistItemContractingBody(
    trackId?: string,
    trackIds?: string[],
): AddPlaylistItemContractingBody {
    if (trackIds !== undefined && trackIds.length > 0) {
        return { trackIds };
    }
    return { trackId };
}
