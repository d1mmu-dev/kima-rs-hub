import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

// Every api call stays pending: the section only needs to render its toggles.
vi.mock("@/lib/api", () => ({
    api: new Proxy({}, { get: () => () => new Promise(() => {}) }),
}));
vi.mock("@/lib/auth-context", () => ({ useAuth: () => ({ user: { role: "admin" } }) }));
vi.mock("@/lib/features-context", () => ({
    useFeatures: () => ({ musicCNN: false, vibeEmbeddings: false, loading: false }),
}));

import { CacheSection } from "../CacheSection";
import type { SystemSettings } from "../../../types";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

let root: ReturnType<typeof createRoot> | null = null;
let el: HTMLElement;
async function render(settings: Partial<SystemSettings>) {
    el = document.createElement("div");
    document.body.appendChild(el);
    root = createRoot(el);
    const client = new QueryClient();
    await act(async () =>
        root!.render(
            <QueryClientProvider client={client}>
                <CacheSection
                    settings={{ autoEnrichMetadata: true, autoEnrichMetadataLocked: false, ...settings } as SystemSettings}
                    onUpdate={vi.fn()}
                />
            </QueryClientProvider>,
        ),
    );
}
const toggle = () => el.querySelector<HTMLInputElement>("#auto-enrich")!;

afterEach(() => {
    act(() => root?.unmount());
    root = null;
    el?.remove();
});

describe("CacheSection auto enrich", () => {
    it("toggle description text", async () => {
        await render({});
        expect(el.textContent).toContain("Look up MusicBrainz, AcoustID, artist info and lyrics online");
        expect(el.textContent).not.toContain("KIMA_IDENTIFY=off");
        expect(toggle().disabled).toBe(false);
    });

    it("toggle disabled with lock note when locked", async () => {
        await render({ autoEnrichMetadata: false, autoEnrichMetadataLocked: true });
        expect(toggle().disabled).toBe(true);
        expect(el.textContent).toContain("Turned off on the server (KIMA_IDENTIFY=off)");
    });
});
