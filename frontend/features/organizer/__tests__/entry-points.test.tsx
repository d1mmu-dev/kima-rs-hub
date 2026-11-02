import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/lib/api";
import AttentionPage from "@/app/settings/attention/page";
import { LibraryToolsSection } from "@/features/settings/components/sections/LibraryToolsSection";

vi.mock("@/lib/auth-context", () => ({ useAuth: () => ({ user: { role: "admin" } }) }));
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
async function render(child: React.ReactNode) {
    qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);
    await act(async () => {
        root.render(<QueryClientProvider client={qc}>{child}</QueryClientProvider>);
    });
    await act(async () => {
        await new Promise((r) => setTimeout(r, 30));
    });
}
it("attention shows Symlinked files label and a Match link per album", async () => {
    vi.spyOn(api, "getAttention").mockResolvedValue([
        { albumId: "a/b", artist: "Artist", album: "Album", year: null, issues: ["symlink"] },
        { albumId: "second", artist: "Band", album: "Second", year: 2000, issues: ["missingYear"] },
    ]);
    await render(<AttentionPage />);
    expect(el.querySelector('option[value="symlink"]')?.textContent).toBe("Symlinked files");
    const rows = [...el.querySelectorAll("tr")].filter((r) => r.querySelector('a[href^="/album"]'));
    expect(rows.length, el.innerHTML).toBe(2);
    expect(rows[0].textContent).toContain("Symlinked files");
    expect(
        rows.map((r) => [...r.querySelectorAll("a")].find((a) => a.textContent === "Match")?.getAttribute("href")),
    ).toEqual(["/settings/autotag/album?albumId=a%2Fb", "/settings/autotag/album?albumId=second"]);
});
it("library tools has a Match albums link", async () => {
    await render(<LibraryToolsSection />);
    const links = [...el.querySelectorAll("a")];
    expect(links.map((a) => a.textContent)).toEqual(["Organize library", "Needs attention", "Match albums"]);
    expect(links[2].getAttribute("href")).toBe("/settings/autotag");
});
