import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import { RunPanel } from "../components/RunPanel";
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
it("shows failed recovery errors and preserves them when Undo is refused", async () => {
    vi.spyOn(api, "getOrganizerRun").mockResolvedValue({
        state: "done",
    } as Awaited<ReturnType<typeof api.getOrganizerRun>>);
    const groups = vi.spyOn(api, "getOrganizerRunAlbums").mockResolvedValue({
        albums: [
            {
                albumId: "a",
                artist: "Artist",
                album: "Planned moves",
                rows: [],
            },
        ],
        total: 1,
    });
    const undo = vi.spyOn(api, "undoAutotagRun").mockRejectedValue(
        Object.assign(new Error("Identity collision"), {
            data: { conflictAlbumId: "other" },
        }),
    );
    vi.spyOn(api, "getAutotagRun").mockResolvedValue({
        id: "r",
        albumId: "a",
        album: "Album",
        artist: "Artist",
        target: "db",
        state: "undo_failed",
        reorganize: false,
        createdAt: null,
        finishedAt: null,
        organizerRunId: "org",
        summary: { needsUndo: true },
        error: "Restore failed",
        rows: [],
        total: 0,
    });
    qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    el = document.createElement("div");
    root = createRoot(el);
    await act(async () =>
        root.render(
            <QueryClientProvider client={qc}>
                <RunPanel id="r" />
            </QueryClientProvider>,
        ),
    );
    await vi.waitFor(async () => {
        await act(async () => {
            await new Promise((r) => setTimeout(r, 0));
        });
        expect(el.textContent).toContain("Planned moves");
    });
    expect(groups).toHaveBeenCalledWith("org", 0, 20);
    expect(el.textContent).toContain("Planned moves");
    expect(el.textContent).toContain("Restore failed");
    expect(el.textContent).toContain("owner decision");
    act(() =>
        [...el.querySelectorAll("button")]
            .find((b) => b.textContent === "Undo")!
            .click(),
    );
    await act(async () => {
        await new Promise((r) => setTimeout(r, 30));
    });
    expect(undo).toHaveBeenCalledWith("r");
    expect(el.textContent).toContain("Identity collision");
    expect(el.querySelector('a[href*="other"]')).not.toBeNull();
    expect(el.textContent).toContain("Restore failed");
});
