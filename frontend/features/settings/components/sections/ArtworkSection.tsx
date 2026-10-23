"use client";

import { SettingsSection, SettingsRow, SettingsInput, SettingsToggle, ConnectionTestButton } from "../ui";
import { ConnectionTestResult, SystemSettings } from "../../types";
import { useConnectionTest } from "../../hooks/useConnectionTest";

interface ArtworkSectionProps {
    settings: SystemSettings;
    onUpdate: (updates: Partial<SystemSettings>) => void;
    onTest: (service: string) => Promise<ConnectionTestResult>;
    isTesting: boolean;
}

const SAVED_PLACEHOLDER = "Saved — type to replace";

/** The stored key's Remove link; `null` in the settings means it was removed and awaits a save. */
function RemoveLink({ onClick }: { onClick: () => void }) {
    return (
        <button
            type="button"
            onClick={onClick}
            className="ml-3 text-xs text-[var(--text-muted)] hover:text-white underline transition-colors"
        >
            Remove
        </button>
    );
}

export function ArtworkSection({ settings, onUpdate, onTest, isTesting }: ArtworkSectionProps) {
    const fanartSaved = settings.hasFanartApiKey && settings.fanartApiKey !== null;
    const acoustidSaved = settings.hasAcoustidApiKey && settings.acoustidApiKey !== null;
    const fanartTest = useConnectionTest(() => onTest("fanart"), { connected: () => "Connected" });

    return (
        <SettingsSection
            id="artwork"
            title="Artwork"
            description="Enhance your library with high-quality artwork"
        >
            <SettingsRow
                label="Enable Fanart.tv"
                description="Enhanced artist and album artwork"
                htmlFor="fanart-enabled"
            >
                <SettingsToggle
                    id="fanart-enabled"
                    checked={settings.fanartEnabled}
                    onChange={(checked) => onUpdate({ fanartEnabled: checked })}
                />
            </SettingsRow>

            {settings.fanartEnabled && (
                <>
                    <SettingsRow label="API Key">
                        <div className="flex items-center">
                            <SettingsInput
                                type="password"
                                value={settings.fanartApiKey ?? ""}
                                onChange={(v) => onUpdate({ fanartApiKey: v })}
                                placeholder={fanartSaved ? SAVED_PLACEHOLDER : "Enter Fanart.tv API key"}
                                className="w-64"
                            />
                            {fanartSaved && <RemoveLink onClick={() => onUpdate({ fanartApiKey: null })} />}
                        </div>
                    </SettingsRow>

                    <div className="pt-2">
                        <ConnectionTestButton connection={fanartTest} disabled={isTesting || !(settings.fanartApiKey || fanartSaved)} />
                    </div>
                </>
            )}

            <div className="mt-6 pt-6 border-t border-white/5">
                <div className="mb-4">
                    <p className="text-xs text-[var(--text-muted)]">
                        AcoustID identifies tracks by their audio fingerprint. Add your own API key for higher rate limits.
                    </p>
                </div>

                <SettingsRow label="AcoustID API Key (Optional)">
                    <div className="flex items-center">
                        <SettingsInput
                            type="password"
                            value={settings.acoustidApiKey || ""}
                            onChange={(v) => onUpdate({ acoustidApiKey: v })}
                            placeholder={acoustidSaved ? SAVED_PLACEHOLDER : "Optional: Your AcoustID API key"}
                            className="w-64"
                        />
                        {acoustidSaved && <RemoveLink onClick={() => onUpdate({ acoustidApiKey: null })} />}
                    </div>
                </SettingsRow>
            </div>
        </SettingsSection>
    );
}
