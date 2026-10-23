import { describe, it, expect } from "vitest";
import { systemSettingsBody } from "../systemSettingsBody";
import type { SystemSettings } from "../types";

const loaded = {
    fanartEnabled: true,
    fanartApiKey: "",
    hasFanartApiKey: true,
    acoustidApiKey: "",
    hasAcoustidApiKey: true,
    autoEnrichMetadata: false,
    autoEnrichMetadataLocked: true,
} as SystemSettings;

describe("systemSettingsBody", () => {
    it("omits an untouched secret instead of sending an empty string", () => {
        const body = systemSettingsBody(loaded);
        expect("fanartApiKey" in body).toBe(false);
        expect("acoustidApiKey" in body).toBe(false);
        expect(JSON.stringify(body)).not.toContain('""');
    });

    it("omits a blank (whitespace) secret", () => {
        const body = systemSettingsBody({ ...loaded, fanartApiKey: "  ", acoustidApiKey: "" });
        expect("fanartApiKey" in body).toBe(false);
        expect("acoustidApiKey" in body).toBe(false);
    });

    it("sends null for a secret only when it was removed", () => {
        const body = systemSettingsBody({ ...loaded, fanartApiKey: null });
        expect(body.fanartApiKey).toBeNull();
        expect("acoustidApiKey" in body).toBe(false);
    });

    it("sends a typed value as it is", () => {
        const body = systemSettingsBody({ ...loaded, acoustidApiKey: "abc123" });
        expect(body.acoustidApiKey).toBe("abc123");
        expect("fanartApiKey" in body).toBe(false);
    });

    it("never posts the read-only flags", () => {
        const body = systemSettingsBody(loaded);
        expect("hasFanartApiKey" in body).toBe(false);
        expect("hasAcoustidApiKey" in body).toBe(false);
        expect("autoEnrichMetadataLocked" in body).toBe(false);
        expect(body.fanartEnabled).toBe(true);
    });
});
