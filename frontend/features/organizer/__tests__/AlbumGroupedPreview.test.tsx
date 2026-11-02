import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it } from "vitest";
import { AlbumGroupedPreview } from "../components/AlbumGroupedPreview";
import type { OrganizerRow } from "../types";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
const row = (id: number): OrganizerRow => ({
    id,
    trackId: `t${id}`,
    from: `/old/${id}.flac`,
    to: `/new/${id}.flac`,
    state: "planned",
    note: null,
    collision: false,
    albumId: "a",
    artist: "Artist",
    album: "Album",
    guessed: ["missingYear"],
});
const groups = [
    { albumId: "a", artist: "Artist", album: "Album", rows: [row(1), row(2)] },
    { albumId: "b", artist: "Band", album: "Second", rows: [{ ...row(3), albumId: "b" }] },
];
let root: ReturnType<typeof createRoot>;
let el: HTMLDivElement;
afterEach(() => {
    act(() => root?.unmount());
    el?.remove();
});
function render(over: Partial<React.ComponentProps<typeof AlbumGroupedPreview>> = {}) {
    el = document.createElement("div");
    document.body.append(el);
    root = createRoot(el);

    const props = { groups, page: 0, total: 2, onPage: () => {} };
    act(() => root.render(<AlbumGroupedPreview {...props} {...over} />));
}
it("groups rows under album headers with counts and issues", () => {
    render();
    const headers = [...el.querySelectorAll("summary")];
    expect(headers).toHaveLength(2);
    expect(headers[0].textContent).toContain("Artist — Album");
    expect(headers[0].textContent).toContain("2 files");
    expect(headers[0].textContent).toContain("No year");
    expect(headers[1].textContent).toContain("Band — Second");
    expect(headers[0].closest("details")!.textContent).toContain("/new/1.flac");
    expect(headers[0].closest("details")!.textContent).not.toContain("/new/3.flac");
});
it("collapses and expands a group", () => {
    render();
    const details = el.querySelector("details")!;
    expect(details.open).toBe(true);
    act(() => details.querySelector("summary")!.click());
    expect(details.open).toBe(false);
    act(() => details.querySelector("summary")!.click());
    expect(details.open).toBe(true);
});
it('sidecar group without album is labelled "Other files"', () => {
    render({ groups: [{ albumId: null, artist: null, album: null, rows: [{ ...row(4), trackId: null }] }], total: 1 });
    expect(el.querySelector("summary")!.textContent).toContain("Other files");
});
it("pages by whole albums and disables boundary controls", () => {
    const pages: number[] = [];
    render({ total: 21, onPage: (n) => pages.push(n) });
    const buttons = [...el.querySelectorAll("button")];
    expect(buttons.map((b) => b.textContent)).toEqual(["Previous", "Next"]);
    expect(buttons[0].disabled).toBe(true);
    act(() => buttons[1].click());
    expect(pages).toEqual([1]);
    act(() =>
        root.render(<AlbumGroupedPreview groups={[groups[1]]} total={21} page={1} onPage={(n) => pages.push(n)} />),
    );
    expect(el.textContent).toContain("21-21 of 21 albums");
    expect(el.querySelectorAll("button")[1].disabled).toBe(true);
    act(() => el.querySelectorAll("button")[0].click());
    expect(pages).toEqual([1, 0]);
});
it("keeps filters independent within each album", () => {
    render();
    const details = el.querySelectorAll("details");
    act(() => details[0].querySelectorAll<HTMLInputElement>("input")[1].click());
    expect(details[0].textContent).toContain("Nothing matches these filters.");
    expect(details[1].textContent).toContain("/new/3.flac");
    expect(details[0].querySelector("summary")!.textContent).toContain("2 files");
});
