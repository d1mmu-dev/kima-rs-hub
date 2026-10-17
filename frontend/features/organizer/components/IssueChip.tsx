import { issueLabel } from "../filters";

export function IssueChip({ code }: { code: string }) {
    return (
        <span className="inline-block rounded border border-white/10 bg-white/5 px-2 py-0.5 text-xs text-[var(--text-secondary)]">
            {issueLabel(code)}
        </span>
    );
}
