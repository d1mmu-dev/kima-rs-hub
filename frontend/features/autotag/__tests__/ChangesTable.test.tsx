import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vitest";
import { ChangesTable } from "../components/ChangesTable";
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
it("hides unchanged fields until toggled", () => {
    const el = document.createElement("div");
    const root = createRoot(el);
    act(() =>
        root.render(
            <ChangesTable
                changes={[
                    {
                        trackId: null,
                        field: "title",
                        current: "Old",
                        proposed: "New",
                    },
                    {
                        trackId: null,
                        field: "year",
                        current: 2005,
                        proposed: 2005,
                    },
                ]}
            />,
        ),
    );
    expect(el.textContent).toContain("Old");
    expect(el.querySelectorAll("tbody tr")).toHaveLength(1);
    act(() => el.querySelector("input")!.click());
    expect(el.querySelectorAll("tbody tr")).toHaveLength(2);
    act(() => root.unmount());
});
