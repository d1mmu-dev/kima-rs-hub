import { describe, expect, it } from "vitest";
import { SETTINGS_PAGES, addressForOldSection, settingsPage } from "../pages";

describe("settings navigation", () => {
    it("keeps the personal page public and separates admin-only pages", () => {
        expect(SETTINGS_PAGES.map((p) => p.slug)).toEqual(["you", "library", "sources", "server", "people"]);
        expect(settingsPage("you").adminOnly).toBe(false);
        expect(SETTINGS_PAGES.slice(1).every((p) => p.adminOnly)).toBe(true);
    });

    it("preserves old section links and local library tools", () => {
        expect(addressForOldSection("lidarr")).toBe("/settings/sources#lidarr");
        expect(addressForOldSection("library-tools")).toBe("/settings/library#library-tools");
        expect(addressForOldSection("account")).toBeUndefined();
        expect(settingsPage("library").sections.map((s) => s.id)).toEqual(["cache", "corrupt-tracks", "library-tools"]);
    });
});
