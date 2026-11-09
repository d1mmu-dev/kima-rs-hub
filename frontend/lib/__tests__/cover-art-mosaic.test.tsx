import { describe, it, expect, afterEach } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import React, { act } from "react";
import { Music } from "lucide-react";
import { CoverArt } from "@/components/cards/MediaCard";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
afterEach(() => { if (root) act(() => root!.unmount()); root = null; document.body.innerHTML = ""; });

function draw(urls: string[]) {
    const host = document.body.appendChild(document.createElement("div"));
    root = createRoot(host);
    act(() => root!.render(<CoverArt imageUrl={urls} title="mix" sizes="200px" FallbackIcon={Music} />));
    return host;
}

describe("upstream cover tile", () => {
    it("deduplicates a mosaic and fits two distinct covers without empty slots", () => {
        const host = draw(["/a.jpg", "/b.jpg", "/a.jpg"]);
        expect([...host.querySelectorAll("img")].map(img => new URL(img.getAttribute("src")!, "http://x").pathname)).toEqual(["/a.jpg", "/b.jpg"]);
        expect(host.querySelectorAll("[data-cover-cell]")).toHaveLength(2);
        expect(host.firstElementChild?.getAttribute("data-cover-count")).toBe("2");
    });
    it("refits when an image fails and falls back when all fail", () => {
        const host = draw(["/a.jpg", "/b.jpg"]);
        act(() => host.querySelector("img")!.dispatchEvent(new Event("error")));
        expect(host.querySelectorAll("img")).toHaveLength(1);
        act(() => host.querySelector("img")!.dispatchEvent(new Event("error")));
        expect(host.querySelectorAll("img")).toHaveLength(0);
        expect(host.querySelectorAll("svg")).toHaveLength(1);
    });
});
