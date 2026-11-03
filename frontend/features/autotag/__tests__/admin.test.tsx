import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import ListPage from "@/app/settings/autotag/page";
import ReviewPage from "@/app/settings/autotag/album/page";
vi.mock("@/lib/auth-context", () => ({
    useAuth: () => ({ user: { role: "user" } }),
}));
vi.mock("next/navigation", () => ({
    useSearchParams: () => new URLSearchParams("albumId=a"),
}));
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
it("never loads admin data for a non-admin on either Match page", async () => {
    const request = vi.spyOn(api, "get");
    const el = document.createElement("div"),
        root = createRoot(el),
        qc = new QueryClient();
    await act(async () =>
        root.render(
            <QueryClientProvider client={qc}>
                <ListPage />
                <ReviewPage />
            </QueryClientProvider>,
        ),
    );
    expect(el.textContent).toContain("Admins only");
    expect(request).not.toHaveBeenCalled();
    act(() => root.unmount());
    qc.clear();
    vi.restoreAllMocks();
});
