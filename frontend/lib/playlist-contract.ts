/** Named wire payload for POST /playlists/:id/items: exactly one selector. */
export type AddPlaylistItemsBody =
    | { trackId: string; trackIds?: never }
    | { trackIds: string[]; trackId?: never };

/** POST /playlists/:id/items/:itemId/move. Null (or omitted) moves first. */
export interface MovePlaylistItemBody {
    afterItemId?: string | null;
}

/** add_items handler reply, not the distinct /items/batch reply. */
export interface AddPlaylistItemsResponse {
    added: number;
    duplicated: boolean;
    duplicates: number;
    rejected: string[];
}
