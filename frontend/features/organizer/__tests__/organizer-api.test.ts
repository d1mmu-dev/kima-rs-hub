import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";


afterEach(() => vi.restoreAllMocks());
it("sends optional album scope without changing whole-library previews", async () => {
    const post = vi.spyOn(api, "post").mockResolvedValue({ runId: "r", jobId: "j" });
    await api.previewOrganizer("move", ["a", "b"]);
    expect(post).toHaveBeenLastCalledWith("/library/organizer/preview", { mode: "move", albumIds: ["a", "b"] });
    await api.previewOrganizer("copy");
    expect(post).toHaveBeenLastCalledWith("/library/organizer/preview", { mode: "copy" });
});
it("requests album pages with the backend albums/total shape", async () => {
    const result = { albums: [{ albumId: null, album: null, artist: null, rows: [] }], total: 21 };
    const get = vi.spyOn(api, "get").mockResolvedValue(result);
    expect(typeof api.getOrganizerRunAlbums).toBe("function");
    expect(await api.getOrganizerRunAlbums("run/1", 20, 20)).toEqual(result);
    expect(get).toHaveBeenLastCalledWith("/library/organizer/runs/run%2F1/albums?offset=20&limit=20");
    await api.getOrganizerRunAlbums("r");
    expect(get).toHaveBeenLastCalledWith("/library/organizer/runs/r/albums?offset=0&limit=20");
});
