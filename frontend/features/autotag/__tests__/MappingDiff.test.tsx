import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it } from "vitest";
import { MappingDiff } from "../components/MappingDiff";
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
it("highlights missing and extra rows using track names", () => {
    const el = document.createElement("div"),
        root = createRoot(el);
    act(() =>
        root.render(
            <MappingDiff
                mapping={[]}
                missing={[{ medium: 1, position: 2, title: "Missing song" }]}
                extra={["t"]}
                tracks={[
                    {
                        id: "t",
                        title: "Extra song",
                        path: "x",
                        disc: 1,
                        trackNo: 3,
                        artist: "A",
                        duration: 1,
                        recordingMbid: null,
                    },
                ]}
            />,
        ),
    );
    expect(el.querySelector('[data-kind="missing"]')?.textContent).toContain(
        "Missing song",
    );
    expect(el.querySelector('[data-kind="extra"]')?.textContent).toContain(
        "Extra song",
    );
    act(() => root.unmount());
});
