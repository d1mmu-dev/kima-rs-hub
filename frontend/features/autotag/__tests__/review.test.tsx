import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import ReviewPage from "@/app/settings/autotag/album/page";
import type { Review } from "../types";
vi.mock("@/lib/auth-context", () => ({
    useAuth: () => ({ user: { role: "admin" } }),
}));
const route = vi.hoisted(() => ({ album: "a" }));
vi.mock("next/navigation", () => ({
    useSearchParams: () => new URLSearchParams(`albumId=${route.album}`),
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
    route.album = "a";
});
const review: Review = {
    album: {
        id: "a",
        artist: "Artist",
        title: "Local",
        year: 2005,
        tracks: [],
    },
    state: "proposed",
    recommendation: "strong",
    error: null,
    candidates: [
        {
            id: "c",
            title: "Release",
            artist: "Artist",
            distance: 0.02,
            recommendation: "strong",
            origin: "search",
            media: [],
            stale: true,
            releaseMbid: "release",
            releaseGroupMbid: "group",
            date: null,
            country: null,
            label: null,
            catalogNumber: null,
            barcode: null,
            trackCount: 0,
            penalties: {},
            fetchedAt: null,
        } as Review["candidates"][number],
    ],
    conflict: {
        albumId: "other",
        artist: "Other",
        title: "Collision",
        year: 2005,
    },
    selected: {
        candidateId: "c",
        changes: [],
        mapping: [],
        missing: [],
        extra: [],
    },
    userMbidKept: true,
};
async function render(data: Review = { ...review, candidates: review.candidates.map(c => ({ ...c, stale: false })) }) {
    vi.spyOn(api, "getAutotagCapabilities").mockResolvedValue({
        identifyEnabled: true,
        files: { available: true, reason: null },
        copy: { available: true, reason: null, copyRunId: "copy" },
        db: { available: true },
    });
    vi.spyOn(api, "getAutotagAlbum").mockResolvedValue(data);
    qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    await act(async () => {
        root.render(
            <QueryClientProvider client={qc}>
                <ReviewPage />
            </QueryClientProvider>,
        );
    });
    await settle();
}
async function settle() {
    await act(async () => {
        await new Promise((r) => setTimeout(r, 30));
    });
}
it("resets candidate selection and target when navigating to a different album", async () => {
    await render();
    act(() =>
        el
            .querySelector("button[aria-pressed]")!
            .dispatchEvent(new MouseEvent("click", { bubbles: true })),
    );
    await settle();
    route.album = "b";
    await act(async () =>
        root.render(
            <QueryClientProvider client={qc}>
                <ReviewPage />
            </QueryClientProvider>,
        ),
    );
    await settle();
    expect(api.getAutotagAlbum).toHaveBeenLastCalledWith("b", undefined);
});
it("links the runtime conflict if apply loses a race", async () => {
    vi.spyOn(api, "applyAutotagAlbum").mockRejectedValue(
        Object.assign(new Error("Taken by Other — Collision"), {
            data: { conflictAlbumId: "raced" },
        }),
    );
    await render();
    act(() =>
        [...el.querySelectorAll("button")]
            .find((b) => b.textContent === "Apply MBIDs only")!
            .click(),
    );
    await settle();
    expect(el.querySelector('a[href*="raced"]')).not.toBeNull();
});
it("searches using literal release reference, manual artist/album, and refresh; skip is explicit", async () => {
    const search = vi
        .spyOn(api, "searchAutotag")
        .mockResolvedValue({ queued: 1 });
    const skip = vi
        .spyOn(api, "skipAutotagAlbum")
        .mockResolvedValue({ state: "skipped" });
    await render(review);
    const input = (label: string, value: string) => {
        const elInput = el.querySelector<HTMLInputElement>(
            `input[aria-label="${label}"]`,
        )!;
        act(() => {
            Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype,
                "value",
            )!.set!.call(elInput, value);
            elInput.dispatchEvent(new Event("input", { bubbles: true }));
        });
    };
    const click = async (label: string) => {
        act(() =>
            [...el.querySelectorAll("button")]
                .find((b) => b.textContent === label)!
                .click(),
        );
        await settle();
    };
    expect(
        el.querySelector('input[aria-label="Release MBID or URL"]'),
    ).not.toBeNull();
    input("Release MBID or URL", "https://musicbrainz.org/release/exact");
    await click("Find release");
    expect(search).toHaveBeenLastCalledWith({
        albumIds: ["a"],
        releaseMbid: "https://musicbrainz.org/release/exact",
    });
    await click("Search artist / album");
    expect(search).toHaveBeenLastCalledWith({
        albumIds: ["a"],
        query: { artist: "Artist", album: "Local" },
    });
    await click("Refresh candidates");
    expect(search).toHaveBeenLastCalledWith({ albumIds: ["a"] });
    await click("Skip");
    expect(skip).toHaveBeenCalledWith("a");
});
it("offers files-only reorganize and warns for copy target", async () => {
    await render();
    expect(el.textContent).toContain("Re-organize after writing");
    const radios = el.querySelectorAll<HTMLInputElement>(
        'input[name="autotag-target"]',
    );
    act(() => radios[1].click());
    expect(el.textContent).toContain(
        "Undo this tag write before undoing the copy run",
    );
    expect(
        el.querySelector<HTMLInputElement>(
            'input[aria-label="Re-organize after writing"]',
        )!.disabled,
    ).toBe(true);
    act(() => radios[0].click());
    expect(
        el.querySelector<HTMLInputElement>(
            'input[aria-label="Re-organize after writing"]',
        )!.disabled,
    ).toBe(false);
});
it("blocks conflicting identity but explicitly applies MBIDs only to db", async () => {
    vi.spyOn(api, "getAutotagRun").mockResolvedValue({
        id: "r",
        albumId: "a",
        album: "Local",
        artist: "Artist",
        target: "db",
        state: "queued",
        reorganize: false,
        createdAt: null,
        finishedAt: null,
        summary: null,
        error: null,
        rows: [],
        total: 0,
    });
    const apply = vi
        .spyOn(api, "applyAutotagAlbum")
        .mockResolvedValue({ runId: "r", jobId: "j" });
    await render();
    const button = (s: string) =>
        [...el.querySelectorAll("button")].find((b) => b.textContent === s)!;
    expect(button("Accept").disabled).toBe(true);
    expect(button("Merge (later)").disabled).toBe(true);
    expect(el.querySelector('a[href*="other"]')?.textContent).toContain(
        "Collision",
    );
    expect(el.textContent).toContain("user-set MusicBrainz");
    act(() => button("Apply MBIDs only").click());
    await settle();
    expect(apply).toHaveBeenCalledWith("a", {
        candidateId: "c",
        target: "db",
        mbidsOnly: true,
        reorganize: false,
    });
    await settle();
    expect(api.getAutotagRun).toHaveBeenCalledWith("r", 0, 200);
});

it("replaces a selected candidate after a new search without accepting stale review data", async () => {
    const old = { ...review, conflict: null, candidates: [{ ...review.candidates[0], stale: false }] };
    const fresh: Review = { ...old, candidates: [{ ...old.candidates[0], id: "new", title: "New release" }], selected: { ...old.selected!, candidateId: "new" } };
    let replaced = false;
    let holdOld = false;
    let finishOld!: (value: Review) => void;
    const oldRequest = new Promise<Review>((resolve) => { finishOld = resolve; });
    const search = vi.spyOn(api, "searchAutotag").mockImplementation(async () => { replaced = true; return { queued: 1 }; });
    const apply = vi.spyOn(api, "applyAutotagAlbum").mockResolvedValue({ runId: "r", jobId: "j" });
    vi.spyOn(api, "getAutotagRun").mockReturnValue(new Promise(() => {}));
    await render();
    vi.mocked(api.getAutotagAlbum).mockImplementation(async (_id, candidate) => {
        if (candidate === "c" && holdOld) return oldRequest;
        if (replaced && candidate === "c") throw new Error("Candidate not found");
        return replaced ? fresh : old;
    });
    await act(async () => { await qc.invalidateQueries({ queryKey: ["autotag-album", "a"] }); });
    await settle();
    act(() => el.querySelector<HTMLButtonElement>("button[aria-pressed]")!.click());
    await settle();
    holdOld = true;
    act(() => { void qc.invalidateQueries({ queryKey: ["autotag-album", "a"] }); });
    await settle();
    expect([...el.querySelectorAll("button")].find(b => b.textContent === "Accept")!.disabled).toBe(true);
    act(() => [...el.querySelectorAll("button")].find(b => b.textContent === "Search artist / album")!.click());
    await settle();
    expect(search).toHaveBeenCalled();
    await vi.waitFor(() => expect(el.textContent).toContain("New release"));
    await act(async () => { finishOld(old); await oldRequest; });
    await settle();
    expect(el.textContent).toContain("New release");
    expect(el.textContent).not.toContain("Candidate not found");
    const accept = [...el.querySelectorAll("button")].find(b => b.textContent === "Accept")!;
    expect(accept.disabled).toBe(false);
    act(() => accept.click());
    await settle();
    expect(apply).toHaveBeenCalledWith("a", expect.objectContaining({ candidateId: "new" }));
    expect(apply).not.toHaveBeenCalledWith("a", expect.objectContaining({ candidateId: "c" }));
});

it("disables both apply paths if a cached review refetch fails", async () => {
    const apply = vi.spyOn(api, "applyAutotagAlbum");
    await render();
    vi.mocked(api.getAutotagAlbum).mockRejectedValue(new Error("review failed"));
    await act(async () => { await qc.invalidateQueries({ queryKey: ["autotag-album", "a"] }); });
    await settle();
    expect(el.textContent).toContain("review failed");
    const button = [...el.querySelectorAll("button")].find(b => b.textContent === "Apply MBIDs only")!;
    expect(button.disabled).toBe(true);
    act(() => button.click());
    expect(apply).not.toHaveBeenCalled();
});

it("requires a stale candidate to be refreshed before either apply action", async () => {
    await render(review);
    const button = [...el.querySelectorAll("button")].find(b => b.textContent === "Apply MBIDs only")!;
    expect(button.disabled).toBe(true);
});

it("recovers when another search removes the selected candidate", async () => {
    const fresh: Review = {
        ...review,
        conflict: null,
        candidates: [{ ...review.candidates[0], id: "new", title: "Replacement", stale: false }],
        selected: { ...review.selected!, candidateId: "new" },
    };
    await render();
    act(() => el.querySelector<HTMLButtonElement>("button[aria-pressed]")!.click());
    await settle();
    vi.mocked(api.getAutotagAlbum).mockImplementation(async (_id, candidate) => {
        if (candidate === "c") throw new Error("Candidate not found");
        return fresh;
    });
    await act(async () => { await qc.invalidateQueries({ queryKey: ["autotag-album", "a"] }); });
    await vi.waitFor(() => expect(el.textContent).toContain("Replacement"));
    expect(el.textContent).not.toContain("Candidate not found");
    expect([...el.querySelectorAll("button")].find(b => b.textContent === "Accept")!.disabled).toBe(false);
});
