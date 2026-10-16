import { describe, expect, it } from "vitest";
import { liveStatusFor, upNext } from "../useLiveReporting";

const q = ["a", "b", "c", "d"].map((id, i) => ({ id, duration: 100 + i }));

describe("upNext", () => {
    it("follows the queue after the current track", () => {
        expect(upNext(q, 1, false, [], "off").map((e) => e.trackId)).toEqual(["c", "d"]);
    });

    it("gives durations in milliseconds", () => {
        expect(upNext(q, 2, false, [], "off")).toEqual([{ trackId: "d", duration: 103_000 }]);
    });

    it("follows the shuffle order when shuffled", () => {
        expect(upNext(q, 3, true, [2, 3, 0, 1], "off").map((e) => e.trackId)).toEqual(["a", "b"]);
    });

    it("wraps round to the start on repeat all", () => {
        expect(upNext(q, 2, false, [], "all").map((e) => e.trackId)).toEqual(["d", "a", "b"]);
    });

    it("is the same track again on repeat one", () => {
        expect(upNext(q, 1, true, [2, 1, 0, 3], "one")).toEqual([{ trackId: "b", duration: 101_000 }]);
    });

    it("stops at the limit", () => {
        expect(upNext(q, 0, false, [], "off", 2).map((e) => e.trackId)).toEqual(["b", "c"]);
    });

    it("is empty when the current track is not in the queue", () => {
        expect(upNext(q, 9, false, [], "off")).toEqual([]);
    });
});

describe("liveStatusFor", () => {
    it("maps what a listener would notice and nothing else", () => {
        expect(liveStatusFor("playing")).toBe("playing");
        expect(liveStatusFor("paused")).toBe("paused");
        expect(liveStatusFor("blocked")).toBe("paused");
        expect(liveStatusFor("idle")).toBe("stopped");
        expect(liveStatusFor("error")).toBe("stopped");
        expect(liveStatusFor("loading")).toBeNull();
        expect(liveStatusFor("buffering")).toBeNull();
        expect(liveStatusFor("recovering")).toBeNull();
    });
});
