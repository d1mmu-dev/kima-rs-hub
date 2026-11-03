import { QueryClient } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { applyAutotagProgress } from "../progress";
it("ignores malformed event payloads without overwriting real progress", () => {
    const qc = new QueryClient();
    qc.setQueryData(["autotag-progress"], { job: "apply", done: 1, total: 2 });
    const invalidate = vi.spyOn(qc, "invalidateQueries");
    for (const data of [
        null,
        {},
        { job: "other", done: 0, total: 1 },
        { job: "apply", done: "1", total: 2 },
    ])
        applyAutotagProgress(qc, data);
    expect(invalidate).not.toHaveBeenCalled();
    expect(qc.getQueryData(["autotag-progress"])).toEqual({
        job: "apply",
        done: 1,
        total: 2,
    });
    qc.clear();
});
it("invalidates lists, reviews and run details and scopes progress by album/run", () => {
    const qc = new QueryClient();
    const invalidate = vi.spyOn(qc, "invalidateQueries");
    applyAutotagProgress(qc, {
        job: "apply",
        albumId: "a",
        runId: "r",
        done: 1,
        total: 2,
        state: "writing",
    });
    for (const key of [
        ["autotag-albums"],
        ["autotag-album", "a"],
        ["autotag-runs"],
        ["autotag-run", "r"],
    ])
        expect(invalidate).toHaveBeenCalledWith({ queryKey: key });
    expect(qc.getQueryData(["autotag-progress", "r"])).toMatchObject({
        done: 1,
        total: 2,
    });
    expect(qc.getQueryData(["autotag-progress", "album", "a"])).toMatchObject({
        job: "apply",
    });
    qc.clear();
});
