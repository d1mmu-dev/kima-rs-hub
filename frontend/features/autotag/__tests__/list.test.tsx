import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import ListPage from "@/app/settings/autotag/page";
vi.mock("@/lib/auth-context", () => ({
    useAuth: () => ({ user: { role: "admin" } }),
}));
vi.mock("@/components/layout/PageHeader", () => ({
    PageHeader: ({ title }: { title: string }) => <h1>{title}</h1>,
}));
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
let root: ReturnType<typeof createRoot>, el: HTMLDivElement, qc: QueryClient;
afterEach(() => {
    act(() => root?.unmount());
    el?.remove();
    qc?.clear();
    vi.restoreAllMocks();
});
it("selects albums for search and explicitly scopes match all unmatched", async () => {
    vi.spyOn(api, "getAutotagCapabilities").mockResolvedValue({
        identifyEnabled: true,
        files: { available: false, reason: "Read only" },
        copy: { available: false, reason: "Copy first", copyRunId: null },
        db: { available: true },
    });
    vi.spyOn(api, "getAutotagAlbums").mockResolvedValue({
        albums: [
            {
                albumId: "a",
                artist: "Artist",
                album: "The Woods",
                state: "proposed",
                recommendation: "strong",
                distance: 0.02,
                trackCount: 2,
                year: 2005,
                issues: [],
                releaseMbid: null,
                searchedAt: null,
                appliedAt: null,
            },
        ],
        total: 1,
    });
    vi.spyOn(api, "getAutotagRuns").mockResolvedValue({ runs: [] });
    const search = vi
        .spyOn(api, "searchAutotag")
        .mockResolvedValue({ queued: 1 });
    qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    el = document.createElement("div");
    root = createRoot(el);
    await act(async () =>
        root.render(
            <QueryClientProvider client={qc}>
                <ListPage />
            </QueryClientProvider>,
        ),
    );
    await settle();
    expect(el.textContent).toContain("Read only");
    expect(el.querySelector('a[href*="albumId=a"]')?.textContent).toContain(
        "The Woods",
    );
    const filters = el.querySelectorAll("select");
    expect(filters).toHaveLength(2);
    act(() => {
        filters[0].value = "proposed";
        filters[0].dispatchEvent(new Event("change", { bubbles: true }));
    });
    await settle();
    act(() => {
        filters[1].value = "strong";
        filters[1].dispatchEvent(new Event("change", { bubbles: true }));
    });
    await settle();
    expect(api.getAutotagAlbums).toHaveBeenLastCalledWith({
        state: "proposed",
        recommendation: "strong",
        q: undefined,
        offset: 0,
        limit: 50,
    });
    const button = (s: string) =>
        [...el.querySelectorAll("button")].find((b) => b.textContent === s)!;
    expect(button("Find matches").disabled).toBe(true);
    act(() =>
        el.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click(),
    );
    act(() => button("Find matches").click());
    await settle();
    expect(search).toHaveBeenLastCalledWith({ albumIds: ["a"] });
    act(() => button("Match all unmatched").click());
    await settle();
    expect(search).toHaveBeenLastCalledWith({ all: true });
});
async function settle() {
    await act(async () => {
        await new Promise((r) => setTimeout(r, 30));
    });
}
