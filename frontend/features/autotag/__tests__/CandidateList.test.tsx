import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { CandidateList } from "../components/CandidateList";
import type { Candidate } from "../types";
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
it("selects a release with score, provenance and penalties", () => {
    const el = document.createElement("div"),
        root = createRoot(el),
        change = vi.fn();
    const candidate = {
        id: "c",
        title: "Release",
        artist: "Artist",
        distance: 0.02,
        recommendation: "strong",
        origin: "tag",
        media: ["CD"],
        penalties: { year: 1 },
    } as Candidate;
    act(() =>
        root.render(
            <CandidateList
                candidates={[candidate]}
                selected="c"
                onSelect={change}
            />,
        ),
    );
    expect(el.textContent).toContain("strong");
    expect(el.textContent).toContain("tag");
    expect(el.querySelector("progress")?.value).toBe(0.02);
    expect(el.querySelector("[title]")?.getAttribute("title")).toContain(
        "year",
    );
    act(() => el.querySelector("button")!.click());
    expect(change).toHaveBeenCalledWith("c");
    act(() => root.unmount());
});
