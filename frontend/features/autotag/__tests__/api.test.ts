import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
afterEach(() => vi.restoreAllMocks());
it("uses exact admin endpoints, encodes identifiers and preserves apply flags", async () => {
    const get = vi.spyOn(api, "get").mockResolvedValue({});
    const post = vi.spyOn(api, "post").mockResolvedValue({});
    expect(typeof api.getAutotagAlbum).toBe("function");
    await api.getAutotagCapabilities();
    expect(get).toHaveBeenLastCalledWith("/library/autotag/capabilities");
    await api.getAutotagAlbums({
        q: "A&B",
        state: "proposed",
        recommendation: "strong",
        offset: 50,
        limit: 50,
    });
    expect(get).toHaveBeenLastCalledWith(
        "/library/autotag/albums?q=A%26B&state=proposed&recommendation=strong&offset=50&limit=50",
    );
    await api.getAutotagAlbum("a/b", "c/d");
    expect(get).toHaveBeenLastCalledWith(
        "/library/autotag/albums/a%2Fb?candidateId=c%2Fd",
    );
    const body = {
        candidateId: "c",
        target: "db" as const,
        reorganize: false,
        mbidsOnly: true,
    };
    await api.applyAutotagAlbum("a/b", body);
    expect(post).toHaveBeenLastCalledWith(
        "/library/autotag/albums/a%2Fb/apply",
        body,
    );
    await api.searchAutotag({ albumIds: ["a"], releaseMbid: "literal" });
    expect(post).toHaveBeenLastCalledWith("/library/autotag/search", {
        albumIds: ["a"],
        releaseMbid: "literal",
    });
    await api.skipAutotagAlbum("a/b");
    expect(post).toHaveBeenLastCalledWith(
        "/library/autotag/albums/a%2Fb/skip",
        {},
    );
    await api.getAutotagRuns(20);
    expect(get).toHaveBeenLastCalledWith(
        "/library/autotag/runs?offset=20&limit=20",
    );
    await api.getAutotagRun("r/s", 200);
    expect(get).toHaveBeenLastCalledWith(
        "/library/autotag/runs/r%2Fs?offset=200&limit=200",
    );
    await api.undoAutotagRun("r/s");
    expect(post).toHaveBeenLastCalledWith(
        "/library/autotag/runs/r%2Fs/undo",
        {},
    );
});
