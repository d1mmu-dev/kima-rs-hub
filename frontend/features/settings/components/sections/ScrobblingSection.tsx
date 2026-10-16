"use client";

import { useState } from "react";
import { SettingsSection, SettingsRow, SettingsInput, SettingsToggle } from "../ui";
import { InlineStatus, StatusType } from "@/components/ui/InlineStatus";
import { api } from "@/lib/api";
import { Button } from "@/components/ui/Button";

interface ScrobblingSectionProps {
    /** Whether a token is already saved (the value itself is never returned). */
    configured: boolean;
    /** Called after a save/clear so the parent refreshes `configured`. */
    onSaved: (configured: boolean) => void;
    /** The public live feed switch; omitted unless this server shares this user's listening. */
    liveSharing?: { checked: boolean; onChange: (checked: boolean) => void };
}

/** Per-user ListenBrainz scrobbling with the user's own optional token. */
export function ScrobblingSection({
    configured,
    onSaved,
    liveSharing,
}: ScrobblingSectionProps) {
    const [token, setToken] = useState("");
    const [clearRequested, setClearRequested] = useState(false);
    const [status, setStatus] = useState<StatusType>("idle");
    const [message, setMessage] = useState("");

    const dirty =
        (token.trim().length > 0 && !clearRequested) ||
        (clearRequested && configured);

    const handleSave = async () => {
        setStatus("loading");
        setMessage("Saving...");
        try {
            await api.updateSettings(
                clearRequested
                    ? { listenbrainzToken: null }
                    : { listenbrainzToken: token.trim() }
            );
            setStatus("success");
            setMessage(clearRequested ? "Token removed" : "Scrobbling on");
            setToken("");
            setClearRequested(false);
            onSaved(!clearRequested);
        } catch {
            setStatus("error");
            setMessage("Failed to save");
        }
    };

    return (
        <SettingsSection
            id="scrobbling"
            title="ListenBrainz Scrobbling"
            description="Mirror your plays to your own ListenBrainz account (optional)"
        >
            {liveSharing && (
                <SettingsRow
                    label="Share what I'm playing"
                    description="Show what you're playing on the shared vibe map."
                    htmlFor="share-live"
                >
                    <SettingsToggle id="share-live" checked={liveSharing.checked} onChange={liveSharing.onChange} />
                </SettingsRow>
            )}

            <SettingsRow
                label="User token"
                description="Create one at listenbrainz.org → Settings → Reset token"
                htmlFor="lb-token"
            >
                <SettingsInput
                    id="lb-token"
                    type="password"
                    value={token}
                    onChange={setToken}
                    placeholder={configured ? "Token saved — type to replace" : "Paste your LB user token"}
                    className="w-64"
                />
            </SettingsRow>

            {configured && (
                <SettingsRow
                    label="Remove token"
                    description="Stop mirroring to ListenBrainz"
                    htmlFor="lb-token-clear"
                >
                    <input
                        id="lb-token-clear"
                        type="checkbox"
                        checked={clearRequested}
                        onChange={(e) => setClearRequested(e.target.checked)}
                        className="h-4 w-4"
                    />
                </SettingsRow>
            )}

            <div className="pt-2">
                <div className="inline-flex items-center gap-3">
                    <Button
                        variant="secondary"
                        size="sm"
                        onClick={handleSave}
                        disabled={!dirty || status === "loading"}
                    >
                        {status === "loading"
                            ? "Saving..."
                            : clearRequested
                              ? "Remove token"
                              : "Save token"}
                    </Button>
                    <InlineStatus
                        status={status}
                        message={message}
                        onClear={() => setStatus("idle")}
                    />
                </div>
            </div>
        </SettingsSection>
    );
}
