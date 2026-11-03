import type { LocalTrack, Mapping, Missing } from "../types";
export function MappingDiff({
    mapping,
    missing,
    extra,
    tracks,
}: {
    mapping: Mapping[];
    missing: Missing[];
    extra: string[];
    tracks: LocalTrack[];
}) {
    return (
        <section>
            <h2>Track mapping</h2>
            <table>
                <thead>
                    <tr>
                        <th>Local</th>
                        <th>Release</th>
                        <th>Distance</th>
                    </tr>
                </thead>
                <tbody>
                    {mapping.map((p) => (
                        <tr key={p.trackId}>
                            <td>
                                {p.local.disc}.{p.local.trackNo} {p.local.title}
                            </td>
                            <td>
                                {p.release.medium}.{p.release.position}{" "}
                                {p.release.title}
                            </td>
                            <td>{p.distance.toFixed(3)}</td>
                        </tr>
                    ))}
                    {missing.map((m, i) => (
                        <tr
                            key={`m${i}`}
                            data-kind="missing"
                            className="bg-amber-900/30"
                        >
                            <td>Missing locally</td>
                            <td>
                                {m.medium}.{m.position} {m.title}
                            </td>
                            <td>—</td>
                        </tr>
                    ))}
                    {extra.map((id) => (
                        <tr
                            key={id}
                            data-kind="extra"
                            className="bg-red-900/30"
                        >
                            <td>
                                {tracks.find((t) => t.id === id)?.title ?? id}
                            </td>
                            <td>Extra local track</td>
                            <td>—</td>
                        </tr>
                    ))}
                </tbody>
            </table>
        </section>
    );
}
