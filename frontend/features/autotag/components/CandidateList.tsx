import type { Candidate } from "../types";
export function CandidateList({
    candidates,
    selected,
    onSelect,
}: {
    candidates: Candidate[];
    selected?: string;
    onSelect: (id: string) => void;
}) {
    return (
        <section>
            <h2>Candidates</h2>
            {!candidates.length && <p>No candidates yet.</p>}
            {candidates.map((c) => (
                <button
                    type="button"
                    className="block w-full rounded border border-white/20 p-3 text-left"
                    key={c.id}
                    aria-pressed={selected === c.id}
                    onClick={() => onSelect(c.id)}
                    title={JSON.stringify(c.penalties)}
                >
                    <strong>
                        {c.artist} — {c.title}
                    </strong>
                    <span className="block">
                        {c.date} · {c.country} · {c.media.join(", ")} ·{" "}
                        {c.label} {c.catalogNumber} · {c.trackCount} tracks
                    </span>
                    <span className="block">
                        Distance {c.distance.toFixed(3)} ·{" "}
                        <b>{c.recommendation}</b> · {c.origin}
                    </span>
                    <progress
                        aria-label={`Distance for ${c.title}`}
                        max={1}
                        value={c.distance}
                    />
                </button>
            ))}
        </section>
    );
}
