/**
 * The shuffle button can act only on music, and never while a vibe queue plays in order.
 */
import { describe, expect, it } from "vitest";
import { shuffleState } from "../shuffle";

describe("shuffleState", () => {
    it("is on for music with no vibe queue", () => {
        expect(shuffleState(true, "track", "idle")).toEqual({ enabled: true, title: "shuffle" });
    });
    it("is off for podcasts and audiobooks", () => {
        expect(shuffleState(true, "podcast", "idle").enabled).toBe(false);
    });
    it("is off, and says why, while a song path or blend plays", () => {
        expect(shuffleState(true, "track", "drift")).toEqual({ enabled: false, title: "a vibe queue plays in order" });
    });
});
