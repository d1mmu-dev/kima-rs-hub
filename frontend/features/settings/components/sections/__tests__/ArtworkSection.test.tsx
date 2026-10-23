import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";

import { ArtworkSection } from "../ArtworkSection";
import type { SystemSettings } from "../../../types";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const base = {
    fanartEnabled: true,
    fanartApiKey: "",
    hasFanartApiKey: false,
    acoustidApiKey: "",
    hasAcoustidApiKey: false,
} as SystemSettings;

let root: ReturnType<typeof createRoot> | null = null;
let el: HTMLElement;
async function render(settings: Partial<SystemSettings>, onUpdate = vi.fn(), isTesting = false) {
    el = document.createElement("div");
    document.body.appendChild(el);
    root = createRoot(el);
    await act(async () =>
        root!.render(
            <ArtworkSection
                settings={{ ...base, ...settings }}
                onUpdate={onUpdate}
                onTest={vi.fn()}
                isTesting={isTesting}
            />,
        ),
    );
    return onUpdate;
}
const placeholders = () => [...el.querySelectorAll("input")].map((i) => i.placeholder);
const link = (name: string) =>
    [...el.querySelectorAll("button")].filter((b) => b.textContent === name);

afterEach(() => {
    act(() => root?.unmount());
    root = null;
    el?.remove();
});

describe("ArtworkSection", () => {
    it("shows Saved placeholder and Remove when a key is stored", async () => {
        await render({ hasFanartApiKey: true, hasAcoustidApiKey: true });
        expect(placeholders().filter((p) => p === "Saved — type to replace")).toHaveLength(2);
        expect(link("Remove")).toHaveLength(2);
    });

    it("shows the usual placeholder and no Remove when no key is stored", async () => {
        await render({});
        expect(placeholders()).not.toContain("Saved — type to replace");
        expect(link("Remove")).toHaveLength(0);
    });

    it("Remove posts null", async () => {
        const onUpdate = await render({ hasFanartApiKey: true, hasAcoustidApiKey: true });
        await act(async () => link("Remove")[0].click());
        expect(onUpdate).toHaveBeenLastCalledWith({ fanartApiKey: null });
        await act(async () => link("Remove")[1].click());
        expect(onUpdate).toHaveBeenLastCalledWith({ acoustidApiKey: null });
    });

    it("Test connection enabled with an empty field when a key is stored", async () => {
        await render({ hasFanartApiKey: true });
        const test = link("Test Connection")[0];
        expect(test.disabled).toBe(false);
    });

    it("Test connection disabled with an empty field and no stored key", async () => {
        await render({});
        expect(link("Test Connection")[0].disabled).toBe(true);
    });
});
