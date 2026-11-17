import { afterEach, expect, expectTypeOf, it, vi } from "vitest";
import { api } from "@/lib/api";
import type { AddPlaylistItemsBody } from "@/lib/playlist-contract";

// Compile-time regressions: the server requires exactly one field, not both or neither.
const one: AddPlaylistItemsBody = { trackId: "track" };
const many: AddPlaylistItemsBody = { trackIds: ["track", "other"] };
// @ts-expect-error Both fields must be rejected by the client contract.
const both: AddPlaylistItemsBody = { trackId: "track", trackIds: ["other"] };
// @ts-expect-error An empty payload must be rejected by the client contract.
const neither: AddPlaylistItemsBody = {};
void both;
void neither;

afterEach(() => vi.unstubAllGlobals());

it.each([
    ["single", one],
    ["batch", many],
] as const)("POSTs the %s named payload to the encoded items route and preserves the actual reply", async (kind, body) => {
    // Handler: kima-api/src/routes/playlists.rs::add_items, not the /items/batch reply.
    const reply = { added: 1, duplicated: true, duplicates: 1, rejected: ["missing"] };
    const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify(reply)));
    vi.stubGlobal("fetch", fetcher);
    const result = kind === "single"
        ? await api.addTrackToPlaylist("list/a b", body)
        : await api.addTracksToPlaylist("list/a b", body);
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls[0][0]).toBe("/api/playlists/list%2Fa%20b/items");
    expect(fetcher.mock.calls[0][1]).toMatchObject({ method: "POST", body: JSON.stringify(body) });
    expect(result).toEqual(reply);
    expectTypeOf(result.duplicated).toEqualTypeOf<boolean>();
});

it.each([null, "anchor/a b"])("POSTs a move with afterItemId=%s using the item ID (null means first)", async (afterItemId) => {
    const reply = { success: true, rank: "0000000000" };
    const fetcher = vi.fn().mockResolvedValue(new Response(JSON.stringify(reply)));
    vi.stubGlobal("fetch", fetcher);
    expect(await api.moveTrackInPlaylist("list/a", "item/b", { afterItemId })).toEqual(reply);
    expect(fetcher.mock.calls[0][0]).toBe("/api/playlists/list%2Fa/items/item%2Fb/move");
    expect(fetcher.mock.calls[0][1]).toMatchObject({ method: "POST", body: JSON.stringify({ afterItemId }) });
});
