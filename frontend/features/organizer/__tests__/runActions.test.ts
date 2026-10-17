import { describe, it, expect } from "vitest";
import { runActions } from "../runActions";
import type { OrganizerRun } from "../types";

const run = (o: Partial<OrganizerRun>) => ({ state: "previewed", mode: "copy" as const, switchedAt: null, ...o });

describe("run button rules", () => {
    it("a preview can be applied, not undone", () => {
        const a = runActions(run({}), false);
        expect([a.canApply, a.canUndo, a.canSwitch]).toEqual([true, false, false]);
        expect(a.applyLabel).toBe("Apply");
    });
    it("a failed apply is resumed", () => {
        const a = runActions(run({ state: "failed", lastAction: "apply" }), false);
        expect([a.canApply, a.canUndo]).toEqual([true, true]);
        expect(a.applyLabel).toBe("Resume apply");
        expect(a.undoLabel).toBe("Undo");
    });
    it("a failed undo is retried, never applied", () => {
        const a = runActions(run({ state: "failed", lastAction: "undo" }), false);
        expect([a.canApply, a.canUndo]).toEqual([false, true]);
        expect(a.undoLabel).toBe("Retry undo");
    });
    it("a finished copy can be switched, a move cannot", () => {
        expect(runActions(run({ state: "done" }), false).canSwitch).toBe(true);
        expect(runActions(run({ state: "done", mode: "move" }), false).canSwitch).toBe(false);
    });
    it("nothing is offered while busy, after a switch, or once undone", () => {
        for (const [r, busy] of [
            [run({ state: "done" }), true],
            [run({ state: "done", switchedAt: "2026-01-01T00:00:00Z" }), false],
            [run({ state: "undone" }), false],
        ] as const) {
            const a = runActions(r, busy);
            expect([a.canApply, a.canUndo, a.canSwitch]).toEqual([false, false, false]);
        }
    });
});
