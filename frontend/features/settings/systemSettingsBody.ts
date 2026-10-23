import type { SystemSettings } from "./types";

const SECRETS = ["fanartApiKey", "acoustidApiKey"] as const;

/**
 * The body of POST /system-settings. A stored secret is never sent back: the server
 * returns it as "" and an untouched field is left out, so it can't be overwritten or
 * blanked. `null` (the Remove link) clears it; a typed value replaces it.
 */
export function systemSettingsBody(settings: SystemSettings): Record<string, unknown> {
    const {
        hasFanartApiKey: _hasFanart,
        hasAcoustidApiKey: _hasAcoustid,
        autoEnrichMetadataLocked: _locked,
        ...body
    } = settings;
    void _hasFanart;
    void _hasAcoustid;
    void _locked;
    for (const key of SECRETS) {
        const value = body[key];
        if (value !== null && (value === undefined || value.trim() === "")) delete body[key];
    }
    return body;
}
