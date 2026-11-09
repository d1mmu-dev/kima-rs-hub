import { afterEach, expect, it } from "vitest";
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Shortcuts } from "@/features/stats/components/Shortcuts";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root | null = null;
afterEach(() => {
    act(() => root?.unmount());
    root = null;
    document.body.innerHTML = "";
});

it("stats shortcuts consume Escape when the help panel is open", () => {
    const host = document.body.appendChild(document.createElement("div"));
    root = createRoot(host);
    act(() => root!.render(<Shortcuts chapters={[{ id: "overview", label: "overview" }]} onJump={() => {}} />));
    act(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "?", bubbles: true, cancelable: true })));
    expect(host.querySelector('[role="dialog"]')).not.toBeNull();
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => window.dispatchEvent(escape));
    expect(escape.defaultPrevented).toBe(true);
    expect(host.querySelector('[role="dialog"]')).toBeNull();
});
