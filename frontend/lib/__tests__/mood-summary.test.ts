import { describe, it, expect } from "vitest";
import { moodShares, moodByGroup } from "@/features/vibe/mapUtils";

/**
 * The collection masthead's mood band and the card dots come from these two
 * counts. The band must add up to the analysed library, unanalysed tracks must
 * not dilute it, and every tie must land on the legend's order so the picture
 * never shuffles between loads.
 */

function track(id: string, dominantMood: string, albumId = "al", artistId = "ar") {
    return { id, dominantMood, albumId, artistId };
}

describe("moodShares", () => {
    it("is empty with nothing analysed", () => {
        expect(moodShares([])).toEqual([]);
        expect(moodShares([track("1", "unanalyzed"), track("2", "")])).toEqual([]);
    });

    it("sizes each mood by its share of the analysed tracks, biggest first", () => {
        const shares = moodShares([
            track("1", "moodRelaxed"),
            track("2", "moodRelaxed"),
            track("3", "moodRelaxed"),
            track("4", "moodHappy"),
            track("5", "moodHappy"),
            track("6", "neutral"),
            track("7", "unanalyzed"),
        ]);
        const byKey = new Map(shares.map((s) => [s.key, s.fraction]));
        // Six analysed: three calm, two upbeat, one no-single-mood.
        expect(byKey.get("moodRelaxed")).toBeCloseTo(3 / 6);
        expect(byKey.get("moodHappy")).toBeCloseTo(2 / 6);
        expect(byKey.get("neutral")).toBeCloseTo(1 / 6);
        expect(shares.reduce((sum, s) => sum + s.fraction, 0)).toBeCloseTo(1);
        expect(shares.map((s) => s.key)).toEqual(["moodRelaxed", "moodHappy", "neutral"]);
    });

    it("keeps the legend's order on equal counts and puts no-single-mood last", () => {
        const shares = moodShares([
            track("1", "moodParty"),
            track("2", "moodSad"),
            track("3", "moodHappy"),
            track("4", "neutral"),
        ]);
        // Legend order is happy, sad, relaxed, aggressive, party, neutral last.
        expect(shares.map((s) => s.key)).toEqual(["moodHappy", "moodSad", "moodParty", "neutral"]);
    });
});

describe("moodByGroup", () => {
    // The caller passes one entry per track with the group's id in `id`
    // (the album or artist it belongs to), as useLibraryVibe does.
    const inAlbum = (albumId: string, mood: string) => ({ id: albumId, dominantMood: mood });

    it("marks a group with the mood most of its tracks share", () => {
        const moods = moodByGroup([
            inAlbum("album-a", "moodSad"),
            inAlbum("album-a", "moodSad"),
            inAlbum("album-a", "moodRelaxed"),
            inAlbum("album-b", "moodHappy"),
            inAlbum("album-b", "moodAggressive"),
        ]);
        expect(moods.get("album-a")).toBe("moodSad");
    });

    it("leaves a group unmarked when the winner is no single mood", () => {
        const moods = moodByGroup([
            inAlbum("album-a", "neutral"),
            inAlbum("album-a", "neutral"),
            inAlbum("album-a", "moodHappy"),
            inAlbum("album-b", "unanalyzed"),
        ]);
        expect(moods.has("album-a")).toBe(false);
        expect(moods.has("album-b")).toBe(false);
    });

    it("breaks ties on the legend's order, moods before no-single-mood", () => {
        const moods = moodByGroup([
            inAlbum("album-a", "moodParty"),
            inAlbum("album-a", "moodHappy"),
            inAlbum("album-b", "neutral"),
            inAlbum("album-b", "moodSad"),
        ]);
        expect(moods.get("album-a")).toBe("moodHappy");
        expect(moods.get("album-b")).toBe("moodSad");
    });
});
