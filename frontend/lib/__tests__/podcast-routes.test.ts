import { describe, expect, it } from "vitest";
import { podcastCategoryHref, podcastShowHref, parsePodcastCategoryId } from "@/features/podcast/routes";

describe("static podcast navigation", () => {
    it("encodes show IDs in the exported query route", () => {
        expect(podcastShowHref("a/b & c")).toBe("/podcasts/show/?id=a%2Fb%20%26%20c");
    });
    it("uses one exported page for chart categories and the top chart", () => {
        expect(podcastCategoryHref(1303)).toBe("/podcasts/category/?id=1303");
        expect(podcastCategoryHref("top")).toBe("/podcasts/category/?id=top");
    });
    it("rejects empty, fractional, negative, and malformed category IDs", () => {
        for (const value of [null, "", "1.5", "-1", "garbage", "Infinity"]) {
            expect(parsePodcastCategoryId(value)).toBeUndefined();
        }
        expect(parsePodcastCategoryId("top")).toBeNull();
        expect(parsePodcastCategoryId("1303")).toBe(1303);
    });
});
