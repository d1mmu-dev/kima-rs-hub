// kima-rs (D-170): hooks that read `/stats/*` or `/whats-new/*` send nothing while the server says it lacks
// those sections, so a page that shows them anyway (collection, the players, the version chip) adds no 404s.
import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import React, { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const api = vi.hoisted(() => ({
    get: vi.fn(async (_path: string) => ({})),
    getUpdateStatus: vi.fn(async () => ({ hasUpdate: false })),
}));
vi.mock("@/lib/api", () => ({ api }));

import { SectionFlagsContext, type SectionFlags } from "@/lib/section-flags";
import { useShelfSummary } from "@/features/library/useShelfSummary";
import { useTrackPlays } from "@/features/stats/useTrackPlays";
import { useUpdateStatus } from "@/hooks/useUpdateStatus";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const flags = (on: boolean): SectionFlags => ({
    podcasts: on, audiobooks: on, discover: on, mixes: on, downloads: on, imports: on, stats: on, changelog: on,
    accountDeletion: on,
});

function Probe() {
    useShelfSummary();
    useTrackPlays("t-1");
    useUpdateStatus();
    return null;
}

let root: Root | null = null;
async function render(on: boolean) {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    root = createRoot(document.createElement("div"));
    await act(async () =>
        root!.render(
            <QueryClientProvider client={client}>
                <SectionFlagsContext.Provider value={flags(on)}>
                    <Probe />
                </SectionFlagsContext.Provider>
            </QueryClientProvider>,
        ),
    );
    await act(async () => {});
}

afterEach(() => {
    act(() => root?.unmount());
    root = null;
    vi.clearAllMocks();
});

const statsCalls = () => api.get.mock.calls.filter(([path]) => String(path).startsWith("/stats"));

describe("hidden stats and changelog sections", () => {
    it("asks nothing of /stats or /whats-new when the server lacks them", async () => {
        await render(false);
        expect(statsCalls()).toEqual([]);
        expect(api.getUpdateStatus).not.toHaveBeenCalled();
    });

    it("asks as upstream does when the server has them", async () => {
        await render(true);
        expect(statsCalls().length).toBeGreaterThan(0);
        expect(api.getUpdateStatus).toHaveBeenCalled();
    });
});
