import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { createRoot } from "react-dom/client";
import React, { act } from "react";

const state = vi.hoisted(() => ({
    keys: [] as { id: string; name: string; createdAt: string; lastUsed: string | null }[],
}));
vi.mock("@/lib/auth-context", () => ({ useAuth: () => ({ user: { username: "alice" } }) }));
vi.mock("@/lib/api", () => ({
    api: {
        get: vi.fn(async (path: string) =>
            path === "/api-keys" ? { apiKeys: state.keys } : { publicUrl: "" },
        ),
        post: vi.fn(),
        delete: vi.fn(),
    },
}));

import { SubsonicSection } from "../SubsonicSection";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

let root: ReturnType<typeof createRoot> | null = null;
async function render() {
    const el = document.createElement("div");
    root = createRoot(el);
    await act(async () => root!.render(<SubsonicSection />));
    return el.textContent ?? "";
}

beforeEach(() => {
    state.keys = [];
});
afterEach(() => {
    act(() => root?.unmount());
    root = null;
});

describe("SubsonicSection", () => {
    it("shows guide and list before any token", async () => {
        const text = await render();
        expect(text).toContain("Setup Guide");
        expect(text).toContain(
            "Use the token as the password. It works with apps that send hashed passwords (token and salt) and with apps that have an API key field.",
        );
        expect(text).toContain("Active Tokens");
        expect(text).toContain("Generate Token");
    });

    it("never used label", async () => {
        state.keys = [{ id: "1", name: "phone", createdAt: "2026-01-02T00:00:00Z", lastUsed: null }];
        const text = await render();
        expect(text).toContain("phone");
        expect(text).toContain("Never used");
    });

    it("password warning shown", async () => {
        const text = await render();
        expect(text).toContain(
            "Changing your password signs out every app that uses these tokens. You will need to create new ones.",
        );
    });
});
