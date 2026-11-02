import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import OrganizePage from "@/app/settings/organize/page";

vi.mock("@/lib/auth-context", () => ({ useAuth: () => ({ user: { role: "admin" } }) }));
vi.mock("@/lib/notify", () => ({ notify: { error: vi.fn() } }));
vi.mock("@/components/layout/PageHeader", () => ({ PageHeader: ({ title }: { title: string }) => <h1>{title}</h1> }));
(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: ReturnType<typeof createRoot>;
let el: HTMLDivElement;
let qc: QueryClient;
afterEach(() => {
    act(() => root?.unmount());
    el?.remove();
    qc?.clear();
    vi.restoreAllMocks();
});
const run = {
    id: "r",
    mode: "copy" as const,
    state: "previewed",
    sourceRoot: "/music",
    targetRoot: "/copy",
    switchedAt: null,
    error: null,
    total: 1,
    rows: [],
    lastAction: "apply" as const,
    createdAt: null,
    finishedAt: null,
    summary: {},
    offset: 0,
    limit: 1,
};
async function render(state = "previewed") {
    vi.spyOn(api, "getOrganizerCapabilities").mockResolvedValue({
        copyAvailable: true,
        moveAvailable: true,
        libraryRoot: "/music",
        targetRoot: "/copy",
    });
    vi.spyOn(api, "getWatchStatus").mockResolvedValue({
        mode: "off",
        active: false,
        lastError: null,
        nextSyncAt: null,
    });
    vi.spyOn(api, "getOrganizerRuns").mockResolvedValue({ runs: [] });
    vi.spyOn(api, "getOrganizerRun").mockResolvedValue({ ...run, state } as Awaited<
        ReturnType<typeof api.getOrganizerRun>
    >);
    const albums = vi
        .spyOn(api, "getOrganizerRunAlbums")
        .mockImplementation(async (_id, offset) => ({
            albums: [
                {
                    albumId: offset ? "b" : "a",
                    artist: "Artist",
                    album: offset ? "Last album" : "First album",
                    rows: [],
                },
            ],
            total: 21,
        }));
    qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["organizer", "runs"], { runs: [{ ...run, state }] });
    el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    await act(async () => {
        root.render(
            <QueryClientProvider client={qc}>
                <OrganizePage />
            </QueryClientProvider>,
        );
    });
    await settle();
    return albums;
}
async function settle() {
    await act(async () => {
        await new Promise((r) => setTimeout(r, 30));
    });
}
it("loads whole album pages instead of paginating journal rows", async () => {
    const albums = await render();
    expect(albums).toHaveBeenCalledWith("r", 0, 20);
    expect(el.querySelector("summary")!.textContent).toContain("First album");
    const next = [...el.querySelectorAll("button")].find((b) => b.textContent === "Next")!;
    act(() => next.click());
    await settle();
    await settle();
    expect(albums).toHaveBeenLastCalledWith("r", 20, 20);
    expect(el.querySelector("summary")!.textContent).toContain("Last album");
});
it("explains superseded previews and refuses obsolete actions", async () => {
    await render("superseded");
    expect(el.textContent).toContain("Out of date — preview again");
    for (const label of ["Apply", "Undo", "Switch library to the organized copy"]) {
        expect([...el.querySelectorAll("button")].find((b) => b.textContent === label)!.disabled).toBe(true);
    }
    expect([...el.querySelectorAll("button")].find((b) => b.textContent === "Preview")!.disabled).toBe(false);
});
it("refreshes album rows when planning finishes", async () => {
    const albums = await render("planning");
    albums.mockResolvedValue({
        albums: [{ albumId: "ready", artist: "Ready", album: "Finished plan", rows: [] }],
        total: 1,
    });
    await act(async () => {
        qc.setQueryData(["organizer", "run", "r"], { ...run, state: "previewed" });
    });
    await settle();
    await settle();
    expect(el.querySelector("summary")!.textContent).toContain("Finished plan");
});
