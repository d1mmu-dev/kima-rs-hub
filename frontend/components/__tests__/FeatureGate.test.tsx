import { describe, it, expect, vi, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";

const state = vi.hoisted(() => ({ features: {} as Record<string, boolean> }));
vi.mock("@/lib/features-context", () => ({ useFeatures: () => state.features }));
vi.mock("@/components/ui/LoadingScreen", () => ({ LoadingScreen: () => <div>loading</div> }));
vi.mock("next/link", () => ({ default: (p: { children: React.ReactNode }) => <a>{p.children}</a> }));

import { FeatureGate } from "../FeatureGate";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

let root: ReturnType<typeof createRoot> | null = null;
function render(features: Record<string, boolean>) {
    state.features = features;
    const el = document.createElement("div");
    root = createRoot(el);
    act(() => root!.render(<FeatureGate flag="podcasts"><p>the page</p></FeatureGate>));
    return el.textContent ?? "";
}

afterEach(() => {
    act(() => root?.unmount());
    root = null;
});

describe("FeatureGate", () => {
    it("renders the page when the flag is on", () => {
        expect(render({ loading: false, podcasts: true })).toBe("the page");
    });
    it("shows the not-available panel when the flag is off", () => {
        const text = render({ loading: false, podcasts: false });
        expect(text).toContain("Not available yet");
        expect(text).not.toContain("the page");
    });
    it("renders neither the page nor the panel while features load", () => {
        const text = render({ loading: true, podcasts: false });
        expect(text).toBe("loading");
    });
});
