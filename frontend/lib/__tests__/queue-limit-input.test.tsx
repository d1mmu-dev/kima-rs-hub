/**
 * The queue-limit box in Settings > Playback clamps only when committed, not
 * per keystroke, so typing "5000" saves 5000. Typed one character at a time.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import React, { act, useState } from "react";
import {
    QueueLimitInput,
    QUEUE_MAX,
    QUEUE_MIN,
} from "@/features/settings/components/sections/PlaybackSection";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;
let saved: number | undefined;
const onCommit = vi.fn();

/** Holds the value the way the settings page does, so the box re-renders from it. */
function Harness({ initial }: { initial: number }) {
    const [value, setValue] = useState<number>(initial);
    return (
        <QueueLimitInput
            value={value}
            onCommit={(v) => {
                onCommit(v);
                saved = v;
                setValue(v);
            }}
        />
    );
}

function input(): HTMLInputElement {
    return container.querySelector("input") as HTMLInputElement;
}

/** Set the input's value the way the browser does, so React's onChange fires. */
function setValue(el: HTMLInputElement, v: string) {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
}

function typeChars(text: string) {
    for (const ch of text) {
        act(() => setValue(input(), input().value + ch));
    }
}

function clear() {
    act(() => setValue(input(), ""));
}

function blur() {
    act(() => {
        input().dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
}

beforeEach(() => {
    saved = undefined;
    onCommit.mockClear();
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => root.render(<Harness initial={10000} />));
});

afterEach(() => {
    act(() => root.unmount());
    container.remove();
});

describe("QueueLimitInput", () => {
    it("keeps 5000 as 5000 when it is typed one character at a time", () => {
        clear();
        typeChars("5000");
        expect(input().value).toBe("5000");
        blur();
        expect(saved).toBe(5000);
        expect(input().value).toBe("5000");
    });

    it("does not clamp while typing, only when the box loses focus", () => {
        clear();
        typeChars("5");
        // Mid-typing the box shows what was typed, not the minimum.
        expect(input().value).toBe("5");
        blur();
        expect(saved).toBe(QUEUE_MIN);
        expect(input().value).toBe(String(QUEUE_MIN));
    });

    it("clamps a number above the maximum on blur", () => {
        clear();
        typeChars("250000");
        blur();
        expect(saved).toBe(QUEUE_MAX);
    });

    it("puts the value in effect back when the box is left empty", () => {
        clear();
        blur();
        expect(saved).toBe(10000);
        expect(input().value).toBe("10000");
    });
});
