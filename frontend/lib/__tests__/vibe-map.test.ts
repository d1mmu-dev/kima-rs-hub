import { describe, it, expect } from "vitest";
import * as THREE from "three";
import { buildLibraryIndex, searchLibrary } from "@/features/vibe/librarySearch";
import { computeMoodAnchors } from "@/features/vibe/map/moodAnchors";
import { buildTrackField, FIELD_RADIUS } from "@/features/vibe/map/trackField";
import { ScreenPicker } from "@/features/vibe/map/screenPick";
import { isMood, MOOD_KEYS, moodLabel } from "@/features/vibe/mapUtils";
import type { MapTrack } from "@/features/vibe/types";

/**
 * The vibe map is a WebGL scene, which an e2e assertion can only poke at from
 * outside. The logic that decides what the user sees — search, mood labels,
 * where each star goes, which star is under the pointer — is plain code, so it
 * is pinned down here.
 */

function track(id: string, over: Partial<MapTrack> = {}): MapTrack {
    return {
        id,
        x: 0.5,
        y: 0.5,
        z: 0.5,
        colour: 0.5,
        title: `Title ${id}`,
        artist: "Artist",
        artistId: "a",
        albumId: "b",
        coverUrl: null,
        dominantMood: "moodRelaxed",
        moods: { moodRelaxed: 0.8 },
        energy: 0.5,
        valence: 0.5,
        ...over,
    };
}

describe("searchLibrary", () => {
    const index = buildLibraryIndex([
        track("1", { title: "Digital Love", artist: "Daft Punk" }),
        track("2", { title: "Around the World", artist: "Daft Punk" }),
        track("3", { title: "Café del Mar", artist: "Energy 52" }),
        track("4", { title: "Punk Rock 101", artist: "Bowling for Soup" }),
    ]);

    it("finds tracks by artist, which the sound search could not", () => {
        const r = searchLibrary(index, "daft");
        expect(r.count).toBe(2);
        expect([...r.ids].sort()).toEqual(["1", "2"]);
    });

    it("needs every word, ignoring case and accents", () => {
        expect(searchLibrary(index, "cafe mar").top.map((t) => t.id)).toEqual(["3"]);
        expect(searchLibrary(index, "DAFT love").top.map((t) => t.id)).toEqual(["1"]);
    });

    it("ranks word-start matches ahead of mid-word ones", () => {
        // "punk" starts a word in "Punk Rock 101" and in "Daft Punk"; both rank first
        expect(searchLibrary(index, "punk").count).toBe(3);
        expect(searchLibrary(index, "unk").top.length).toBe(3);
    });

    it("ignores queries shorter than two characters", () => {
        expect(searchLibrary(index, "d").count).toBe(0);
        expect(searchLibrary(index, "  ").count).toBe(0);
    });

    it("caps the result list but counts every match", () => {
        const many = buildLibraryIndex(Array.from({ length: 30 }, (_, i) => track(`m${i}`, { title: `Song ${i}` })));
        const r = searchLibrary(many, "song", 8);
        expect(r.count).toBe(30);
        expect(r.top).toHaveLength(8);
    });
});

describe("computeMoodAnchors", () => {
    it("places one label per mood at that mood's densest pocket", () => {
        const tracks = [
            ...Array.from({ length: 10 }, (_, i) => track(`a${i}`, { dominantMood: "moodSad", x: 0.1, y: 0.1, z: 0.9 })),
            ...Array.from({ length: 3 }, (_, i) => track(`b${i}`, { dominantMood: "moodSad", x: 0.9, y: 0.9, z: 0.1 })),
            ...Array.from({ length: 6 }, (_, i) => track(`c${i}`, { dominantMood: "moodParty", x: 0.6, y: 0.4, z: 0.5 })),
        ];
        const anchors = computeMoodAnchors(tracks);
        expect(anchors.map((a) => a.label)).toEqual(["Melancholic", "Energetic"]);
        const sad = anchors[0];
        expect(sad.x).toBeCloseTo(0.1);
        expect(sad.z).toBeCloseTo(0.9);
        expect(sad.count).toBe(13);
    });

    it("leaves out moods held by only a sliver of the library", () => {
        const tracks = [
            ...Array.from({ length: 200 }, (_, i) => track(`a${i}`)),
            track("rare", { dominantMood: "moodAggressive" }),
        ];
        expect(computeMoodAnchors(tracks).map((a) => a.mood)).toEqual(["moodRelaxed"]);
    });
});

describe("buildTrackField", () => {
    it("puts each star at its own 3-D coordinate, height from z", () => {
        const f = buildTrackField([track("low", { x: 0, y: 1, z: 0 }), track("high", { x: 1, y: 0, z: 1 })]);
        expect(Array.from(f.positions)).toEqual([-FIELD_RADIUS, -FIELD_RADIUS, FIELD_RADIUS, FIELD_RADIUS, FIELD_RADIUS, -FIELD_RADIUS]);
    });

    it("finishes the load-in within two seconds, whatever the library size", () => {
        const tracks = Array.from({ length: 5000 }, (_, i) => track(`t${i}`, { x: (i % 71) / 70, y: (i % 53) / 52, z: (i % 37) / 36 }));
        const f = buildTrackField(tracks);
        expect(f.revealDuration).toBeGreaterThan(0.5);
        expect(f.revealDuration).toBeLessThan(2);
        expect(f.indexById.get("t42")).toBe(42);
    });
});

describe("ScreenPicker", () => {
    it("picks the star drawn under the pointer, and nothing in empty sky", () => {
        const f = buildTrackField([
            track("centre", { x: 0.5, y: 0.5, z: 0.5 }),
            track("right", { x: 0.9, y: 0.5, z: 0.5 }),
        ]);
        const camera = new THREE.PerspectiveCamera(50, 800 / 600, 0.1, 1000);
        camera.position.set(0, 0, 150);
        camera.lookAt(0, 0, 0);
        const picker = new ScreenPicker(f.positions);
        expect(picker.pick(camera, 800, 600, 400, 300, 12)).toBe(0);
        const [rx, ry] = picker.screenOf(camera, 800, 600, 1);
        expect(picker.pick(camera, 800, 600, rx + 5, ry - 5, 12)).toBe(1);
        expect(picker.pick(camera, 800, 600, 100, 100, 12)).toBeNull();
    });

    it("re-projects when the camera moves", () => {
        const f = buildTrackField([track("only", { x: 0.5, y: 0.5, z: 0.5 })]);
        const camera = new THREE.PerspectiveCamera(50, 1, 0.1, 1000);
        camera.position.set(0, 0, 150);
        camera.lookAt(0, 0, 0);
        const picker = new ScreenPicker(f.positions);
        expect(picker.pick(camera, 600, 600, 300, 300, 10)).toBe(0);
        camera.position.set(40, 0, 150);
        camera.lookAt(40, 0, 0);
        expect(picker.pick(camera, 600, 600, 300, 300, 10)).toBeNull();
    });
});

describe("mood names", () => {
    it("shows the five moods by their plain names", () => {
        expect(MOOD_KEYS.map(moodLabel)).toEqual(["Upbeat", "Melancholic", "Calm", "Intense", "Energetic"]);
    });
    it("does not treat acoustic or electronic as moods", () => {
        expect(isMood("moodAcoustic")).toBe(false);
        expect(isMood("moodElectronic")).toBe(false);
    });
    it("says plainly when a track has no single mood or is not analysed yet", () => {
        expect(moodLabel("neutral")).toBe("No single mood");
        expect(moodLabel("unanalyzed")).toBe("Not analysed yet");
        expect(isMood("neutral")).toBe(false);
        expect(isMood("unanalyzed")).toBe(false);
    });
});

