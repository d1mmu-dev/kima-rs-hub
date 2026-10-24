import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";

// The server as it answers before and after a key is saved.
const server = vi.hoisted(() => ({
    settings: {
        fanartEnabled: true,
        fanartApiKey: null as string | null,
        acoustidApiKey: null as string | null,
        hasFanartApiKey: false,
        hasAcoustidApiKey: false,
    },
}));

vi.mock("@/lib/api", () => ({
    api: {
        getSystemSettings: vi.fn(async () => ({ ...server.settings })),
        getSettings: vi.fn(async () => ({ maxCacheSizeMb: 10240 })),
        updateSystemSettings: vi.fn(async (body: Record<string, unknown>) => {
            if (typeof body.fanartApiKey === "string") {
                // Stored: answered back as "" with the flag set.
                server.settings = { ...server.settings, fanartApiKey: "", hasFanartApiKey: true };
            }
            return {};
        }),
        updateSettings: vi.fn(async () => ({})),
    },
}));
vi.mock("@/lib/auth-context", () => ({
    useAuth: () => ({ isAuthenticated: true, user: { role: "admin" } }),
}));

import { useSystemSettings } from "../hooks/useSystemSettings";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

type Hook = ReturnType<typeof useSystemSettings>;
let root: ReturnType<typeof createRoot> | null = null;

function Probe({ onRender }: { onRender: (hook: Hook) => void }) {
    onRender(useSystemSettings());
    return null;
}

async function mount(): Promise<{ current: () => Hook }> {
    let latest: Hook | null = null;
    const el = document.createElement("div");
    document.body.appendChild(el);
    root = createRoot(el);
    await act(async () => root!.render(<Probe onRender={(hook) => (latest = hook)} />));
    return { current: () => latest! };
}

describe("useSystemSettings", () => {
    afterEach(() => {
        act(() => root?.unmount());
        root = null;
    });

    // Final review F3: a first-time key shows as saved (and removable) without a reload.
    it("refreshes the saved-key flags after a save", async () => {
        const hook = await mount();
        expect(hook.current().systemSettings.hasFanartApiKey).toBe(false);

        await act(async () => hook.current().updateSystemSettings({ fanartApiKey: "new-key-1234" }));
        const typed = hook.current().systemSettings;
        await act(async () => {
            await hook.current().saveSystemSettings(typed);
        });

        const after = hook.current().systemSettings;
        expect(after.hasFanartApiKey).toBe(true);
        expect(after.fanartApiKey).toBe("");
        expect(after.fanartEnabled).toBe(true);
    });
});
