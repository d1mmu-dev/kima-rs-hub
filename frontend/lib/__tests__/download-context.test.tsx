/**
 * An album can be requested again once its earlier job has come and gone: the
 * pending list forgets an entry when the job shows up, and so does the guard
 * against double clicks.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import React, { act, useEffect } from "react";

let active: Array<{ targetMbid: string; status: string }> = [];
vi.mock("@/hooks/useEventSource", () => ({ useEventSource: () => {} }));
vi.mock("@/hooks/useNotifications", () => ({ useActiveDownloads: () => ({ downloads: active }) }));

import { DownloadProvider, useDownloadContext } from "@/lib/download-context";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;
const probe = {} as { ctx: ReturnType<typeof useDownloadContext> };

function Probe() {
    const value = useDownloadContext();
    useEffect(() => {
        probe.ctx = value;
    });
    return null;
}

async function render() {
    await act(async () => {
        root.render(
            <DownloadProvider>
                <Probe />
            </DownloadProvider>
        );
    });
}

beforeEach(() => {
    active = [];
    container = document.createElement("div");
    root = createRoot(container);
});

afterEach(() => {
    act(() => root.unmount());
});

describe("DownloadProvider", () => {
    it("ignores a second click on an album already pending", async () => {
        await render();
        let first: string | null = null;
        let second: string | null = "unset";
        await act(async () => {
            first = probe.ctx.addPendingDownload("album", "Antics", "rg-antics");
            second = probe.ctx.addPendingDownload("album", "Antics", "rg-antics");
        });
        expect(first).not.toBeNull();
        expect(second).toBeNull();
    });

    it("accepts the album again after its job has come and gone", async () => {
        await render();
        await act(async () => {
            probe.ctx.addPendingDownload("album", "Antics", "rg-antics");
        });

        active = [{ targetMbid: "rg-antics", status: "processing" }];
        await render();
        expect(probe.ctx.pendingDownloads).toHaveLength(0);

        active = [];
        await render();
        let again: string | null = null;
        await act(async () => {
            again = probe.ctx.addPendingDownload("album", "Antics", "rg-antics");
        });
        expect(again).not.toBeNull();
        expect(probe.ctx.isPendingByMbid("rg-antics")).toBe(true);
    });
});
