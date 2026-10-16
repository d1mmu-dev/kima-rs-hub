import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { api } from "../api";

// Detail-page ids come from decoded query params (D-011), so they can hold
// "/", "..", "?" or "#". Every id must reach the API as one encoded path
// segment, or a crafted link can aim an authenticated request at another
// endpoint (client-side path traversal).

const EVIL = "../x";
const EVIL_ENC = "..%2Fx";

let fetchMock: ReturnType<typeof vi.fn>;

function ok(): Response {
    return {
        ok: true,
        status: 200,
        statusText: "OK",
        headers: { entries: () => [] } as unknown as Headers,
        json: async () => ({}),
    } as unknown as Response;
}

function requestedPath(): string {
    expect(fetchMock).toHaveBeenCalledTimes(1);
    return String(fetchMock.mock.calls[0][0]);
}

beforeEach(() => {
    vi.stubGlobal("window", {});
    vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {}, removeItem: () => {} });
    fetchMock = vi.fn(async () => ok());
    vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
    vi.unstubAllGlobals();
});

describe("ApiClient encodes path parameters", () => {
    it.each<[string, () => Promise<unknown>, string]>([
        ["getPlaylist", () => api.getPlaylist(EVIL), `/api/playlists/${EVIL_ENC}`],
        ["deletePlaylist", () => api.deletePlaylist(EVIL), `/api/playlists/${EVIL_ENC}`],
        ["saveMixAsPlaylist", () => api.saveMixAsPlaylist("a/../b"), "/api/mixes/a%2F..%2Fb/save"],
        ["getMix", () => api.getMix(EVIL), `/api/mixes/${EVIL_ENC}`],
        ["getAlbum", () => api.getAlbum(EVIL), `/api/library/albums/${EVIL_ENC}`],
        ["getAudiobook", () => api.getAudiobook(EVIL), `/api/audiobooks/${EVIL_ENC}`],
        ["getAudiobookSeries", () => api.getAudiobookSeries("A/B Saga"), "/api/audiobooks/series/A%2FB%20Saga"],
        ["getPodcast", () => api.getPodcast("x?y#z"), "/api/podcasts/x%3Fy%23z"],
        ["previewPodcast", () => api.previewPodcast(EVIL), `/api/podcasts/preview/${EVIL_ENC}`],
        ["removePodcast", () => api.removePodcast(EVIL), `/api/podcasts/${EVIL_ENC}/unsubscribe`],
        ["getSimilarPodcasts", () => api.getSimilarPodcasts(EVIL), `/api/podcasts/${EVIL_ENC}/similar`],
        [
            "removeTrackFromPlaylist",
            () => api.removeTrackFromPlaylist(EVIL, EVIL),
            `/api/playlists/${EVIL_ENC}/items/${EVIL_ENC}`,
        ],
        ["revokeShareLink", () => api.revokeShareLink(EVIL), `/api/share/${EVIL_ENC}`],
    ])("%s", async (_name, call, expected) => {
        await call();
        expect(requestedPath()).toBe(expected);
    });

    it("stream URL builders encode ids", () => {
        expect(api.getStreamUrl(EVIL)).toBe(`/api/library/tracks/${EVIL_ENC}/stream`);
        expect(api.getAudiobookStreamUrl(EVIL)).toBe(`/api/audiobooks/${EVIL_ENC}/stream`);
        expect(api.getPodcastEpisodeStreamUrl(EVIL, EVIL)).toBe(
            `/api/podcasts/${EVIL_ENC}/episodes/${EVIL_ENC}/stream`
        );
    });
});
