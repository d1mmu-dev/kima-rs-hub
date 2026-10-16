import { describe, expect, it } from "vitest";
import { isMbid } from "../mbid";

describe("isMbid", () => {
    it("accepts a MusicBrainz id", () => {
        expect(isMbid("42faad37-8aaa-42e4-a300-5a7dae79ed24")).toBe(true);
    });

    it.each([null, undefined, "", "temp-42faad37", "clx0abcd0000abcd"])("refuses %p", (value) => {
        expect(isMbid(value)).toBe(false);
    });
});
