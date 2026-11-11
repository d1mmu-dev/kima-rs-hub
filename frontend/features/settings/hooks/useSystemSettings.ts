import { useState, useEffect, useLayoutEffect, useRef, useCallback } from "react";
import { api } from "@/lib/api";
import { settleFor } from "./settleFor";
import { useAuth } from "@/lib/auth-context";
import { ConnectionTestResult, SystemSettings } from "../types";
import { systemSettingsBody } from "../systemSettingsBody";

const defaultSystemSettings: SystemSettings = {
    lidarrEnabled: true,
    allowMemberAcquire: false,
    lidarrUrl: "http://localhost:8686",
    lidarrApiKey: "",
    fanartEnabled: false,
    fanartApiKey: "",
    acoustidApiKey: "",
    hasFanartApiKey: false,
    hasAcoustidApiKey: false,
    audiobookshelfEnabled: false,
    audiobookshelfUrl: "http://localhost:13378",
    audiobookshelfApiKey: "",
    soulseekUsername: "",
    soulseekPassword: "",
    soulseekMode: "p2p",
    slskdUrl: "",
    slskdApiKey: "",
    musicPath: "/music",
    transcodeCacheMaxGb: 10,
    maxQueueLength: 10000,
    autoSync: true,
    autoEnrichMetadata: true,
    autoEnrichMetadataLocked: false,
    soulseekConcurrentDownloads: 4,
    lidarrQualityProfileId: null,
    lidarrMetadataProfileId: null,
    lidarrRemoveJunk: true,
    // Download preferences
    downloadSource: "soulseek",
    primaryFailureFallback: "none",
    // Server
    publicUrl: "",
    registrationOpen: false,
    updateChecksEnabled: true,
    podcastChartCountry: "us",
};

/** The fields a save changes on the server side: the stored keys and their flags. */
const SECRET_FIELDS = [
    "fanartApiKey",
    "acoustidApiKey",
    "hasFanartApiKey",
    "hasAcoustidApiKey",
] as const satisfies readonly (keyof SystemSettings)[];

export function useSystemSettings() {
    const { isAuthenticated, user } = useAuth();
    const [systemSettings, setSystemSettings] = useState<SystemSettings>(
        defaultSystemSettings
    );
    const [isLoading, setIsLoading] = useState(true);
    const [loadError, setLoadError] = useState<string | null>(null);

    const isAdmin = user?.role === "admin";
    // Loaded for the signed-in admin; a different person starts from the defaults.
    const adminId = isAuthenticated && isAdmin ? (user?.id ?? null) : null;

    // An answer that arrives after someone else signed in is dropped.
    const currentAdminRef = useRef(adminId);
    useLayoutEffect(() => {
        currentAdminRef.current = adminId;
    }, [adminId]);

    // The music folder as loaded: a save sends it only when it was changed here, so a tab left open does not put
    // back a folder someone else has since changed.
    const loadedMusicPathRef = useRef<string | null>(null);

    const fetchSystemSettings = useCallback(() => {
        const askedFor = adminId;
        const current = () => currentAdminRef.current === askedFor;
        // Null strings become empty strings for controlled inputs.
        const show = (sysData: Record<string, unknown>) => {
            const sanitized: Record<string, unknown> = {};
            for (const key in sysData) {
                const value = sysData[key];
                const wantsString = typeof defaultSystemSettings[key as keyof SystemSettings] === "string";
                sanitized[key] = value === null && wantsString ? "" : value;
            }
            const loaded = sanitized as unknown as SystemSettings;
            loadedMusicPathRef.current = loaded.musicPath;
            setSystemSettings(loaded);
        };
        return settleFor(api.getSystemSettings(), current, show, setLoadError, setIsLoading);
    }, [adminId]);

    const loadSystemSettings = () => {
        setIsLoading(true);
        setLoadError(null);
        return fetchSystemSettings();
    };

    const [shownFor, setShownFor] = useState(adminId);
    if (shownFor !== adminId) {
        setShownFor(adminId);
        setSystemSettings(defaultSystemSettings);
        setIsLoading(true);
        setLoadError(null);
    }

    useEffect(() => {
        if (adminId) {
            fetchSystemSettings();
        }
    }, [adminId, fetchSystemSettings]);

    const saveSystemSettings = async (settingsToSave: SystemSettings) => {
        // Upstream sends the music folder only when it changed; kima-rs also never echoes stored keys back.
        const { musicPath, ...rest } = systemSettingsBody(settingsToSave);
        const changedFolder = musicPath !== loadedMusicPathRef.current;
        await api.updateSystemSettings(changedFolder ? { musicPath, ...rest } : rest);
        if (changedFolder) loadedMusicPathRef.current = settingsToSave.musicPath;
        // The saved-key flags (and the keys, answered back as "") come from the server: read
        // them again so a first-time key shows as saved, with Remove, without a reload.
        try {
            const fresh = await api.getSystemSettings();
            setSystemSettings((prev) => {
                const next = { ...prev };
                for (const key of SECRET_FIELDS) {
                    if (key in fresh) {
                        const value = fresh[key];
                        (next as Record<string, unknown>)[key] =
                            value === null && typeof defaultSystemSettings[key] === "string" ? "" : value;
                    }
                }
                return next;
            });
        } catch (error) {
            console.error("Failed to reload the saved keys:", error);
        }
    };

    const updateSystemSettings = (updates: Partial<SystemSettings>) => {
        setSystemSettings((prev) => ({ ...prev, ...updates }));
    };

    /** Tests a service connection; the caller shows the result. */
    const testService = async (service: string): Promise<ConnectionTestResult> => {
        try {
            let result;
            switch (service) {
                case "lidarr":
                    result = await api.testLidarr(
                        systemSettings.lidarrUrl,
                        systemSettings.lidarrApiKey
                    );
                    break;
                case "fanart":
                    result = await api.testFanart(systemSettings.fanartApiKey);
                    break;
                case "audiobookshelf":
                    result = await api.testAudiobookshelf(
                        systemSettings.audiobookshelfUrl,
                        systemSettings.audiobookshelfApiKey
                    );
                    break;
                case "soulseek":
                    result = await api.testSoulseek({
                        mode: systemSettings.soulseekMode === "slskd" ? "slskd" : "p2p",
                        username: systemSettings.soulseekUsername,
                        password: systemSettings.soulseekPassword,
                        slskdUrl: systemSettings.slskdUrl ?? undefined,
                        slskdApiKey: systemSettings.slskdApiKey ?? undefined,
                    });
                    break;
                default:
                    throw new Error(`Unknown service: ${service}`);
            }

            return { success: true, version: result?.version };
        } catch (error: unknown) {
            console.error(`Failed to test ${service}:`, error);
            return { success: false, error: error instanceof Error ? error.message : `Failed to connect` };
        }
    };

    return {
        systemSettings,
        isLoading,
        loadError,
        updateSystemSettings,
        saveSystemSettings,
        testService,
        loadSystemSettings,
    };
}
