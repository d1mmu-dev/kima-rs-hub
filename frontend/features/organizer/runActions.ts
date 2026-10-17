import type { OrganizerRun } from "./types";

export interface RunActions {
    canApply: boolean;
    canUndo: boolean;
    canSwitch: boolean;
    applyLabel: string;
    undoLabel: string;
}

type RunLike = Pick<OrganizerRun, "state" | "mode" | "switchedAt" | "lastAction">;

/** Which buttons a run offers. A failed undo is retried with Undo; it can never be applied again. */
export function runActions(run: RunLike, busy: boolean): RunActions {
    const open = !busy && !run.switchedAt;
    const failedUndo = run.state === "failed" && run.lastAction === "undo";
    return {
        canApply: open && (run.state === "previewed" || (run.state === "failed" && !failedUndo)),
        canUndo: open && (run.state === "done" || run.state === "failed"),
        canSwitch: open && run.mode === "copy" && run.state === "done",
        applyLabel: run.state === "failed" ? "Resume apply" : "Apply",
        undoLabel: failedUndo ? "Retry undo" : "Undo",
    };
}
