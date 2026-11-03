import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { TargetPicker } from "../components/TargetPicker";
(
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
it("disables unavailable targets with their reason", () => {
    const el = document.createElement("div"),
        root = createRoot(el);
    act(() =>
        root.render(
            <TargetPicker
                value="db"
                onChange={vi.fn()}
                capabilities={{
                    identifyEnabled: false,
                    files: { available: false, reason: "Read only" },
                    copy: {
                        available: false,
                        reason: "Copy first",
                        copyRunId: null,
                    },
                    db: { available: true },
                }}
            />,
        ),
    );
    const radios = el.querySelectorAll<HTMLInputElement>("input");
    expect(radios[0].disabled).toBe(true);
    expect(radios[1].disabled).toBe(true);
    expect(radios[2].disabled).toBe(false);
    expect(el.textContent).toContain("Read only");
    expect(el.textContent).toContain("Copy first");
    act(() => root.unmount());
});
