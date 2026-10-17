import { describe, it, expect, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { filterRows, NO_FILTERS } from "../filters";
import { applyLibraryProgress, progressPercent } from "../progress";
import { ProgressBar } from "../components/ProgressBar";
import type { OrganizerRow } from "../types";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

function row(over: Partial<OrganizerRow>): OrganizerRow {
    return {
        id: "r",
        trackId: "t",
        from: "/a/1.flac",
        to: "/b/1.flac",
        state: "planned",
        note: null,
        collision: false,
        albumId: "al",
        album: "Album",
        artist: "Artist",
        guessed: [],
        ...over,
    };
}

describe("organizer table filters", () => {
    const rows = [
        row({ id: "same", state: "unchanged", to: "/a/1.flac" }),
        row({ id: "moved" }),
        row({ id: "clash", collision: true }),
        row({ id: "guess", guessed: ["missingYear"] }),
    ];
    const ids = (f: Partial<typeof NO_FILTERS>) => filterRows(rows, { ...NO_FILTERS, ...f }).map((r) => r.id);

    it("shows everything with no filter", () => {
        expect(ids({})).toEqual(["same", "moved", "clash", "guess"]);
    });
    it("only changes drops unchanged rows", () => {
        expect(ids({ onlyChanges: true })).toEqual(["moved", "clash", "guess"]);
    });
    it("collisions keeps only collisions", () => {
        expect(ids({ collisions: true })).toEqual(["clash"]);
    });
    it("guessed keeps only rows with attention codes", () => {
        expect(ids({ guessed: true })).toEqual(["guess"]);
    });
    it("filters combine", () => {
        expect(ids({ collisions: true, guessed: true })).toEqual([]);
    });
});

/** react-query notifies observers on a timer, so let it fire inside act. */
async function send(qc: QueryClient, event: unknown) {
    await act(async () => {
        applyLibraryProgress(qc, event);
        await new Promise((r) => setTimeout(r, 10));
    });
}

describe("progress event updates bar", () => {
    let root: ReturnType<typeof createRoot> | null = null;
    afterEach(() => {
        act(() => root?.unmount());
        root = null;
    });

    it("computes percentages", () => {
        expect(progressPercent(undefined)).toBe(0);
        expect(progressPercent({ done: 1, total: 0 })).toBe(0);
        expect(progressPercent({ done: 5, total: 10 })).toBe(50);
        expect(progressPercent({ done: 12, total: 10 })).toBe(100);
    });

    it("moves the bar when a library:progress event arrives", async () => {
        const qc = new QueryClient();
        const el = document.createElement("div");
        document.body.appendChild(el);
        root = createRoot(el);
        act(() => {
            root!.render(
                <QueryClientProvider client={qc}>
                    <ProgressBar job="organize" label="Organizing" />
                </QueryClientProvider>,
            );
        });
        const bar = () => el.querySelector('[role="progressbar"]')!.getAttribute("aria-valuenow");
        expect(bar()).toBe("0");

        await send(qc, { type: "library:progress", job: "organize", done: 3, total: 4 });
        expect(bar()).toBe("75");

        // A scan event is a different job and leaves this bar alone; junk is ignored.
        await send(qc, { type: "library:progress", job: "scan", done: 1, total: 4 });
        await send(qc, { type: "library:progress", job: "organize", done: "x" });
        expect(bar()).toBe("75");
        expect(el.textContent).toContain("3 / 4");
    });
});
