"use client";
import { useState } from "react";
import type { Change } from "../types";
const text = (v: unknown) =>
    typeof v === "string" ? v : (JSON.stringify(v) ?? "—");
export function ChangesTable({ changes }: { changes: Change[] }) {
    const [show, setShow] = useState(false);
    return (
        <section>
            <h2>Changes</h2>
            <label>
                <input
                    type="checkbox"
                    checked={show}
                    onChange={(e) => setShow(e.target.checked)}
                />{" "}
                Show unchanged
            </label>
            <table>
                <thead>
                    <tr>
                        <th>Track / field</th>
                        <th>Current</th>
                        <th>Proposed</th>
                    </tr>
                </thead>
                <tbody>
                    {changes
                        .filter(
                            (c) =>
                                show ||
                                JSON.stringify(c.current) !==
                                    JSON.stringify(c.proposed),
                        )
                        .map((c, i) => (
                            <tr key={i}>
                                <th>
                                    {c.trackId ?? "Album"} / {c.field}
                                </th>
                                <td>{text(c.current)}</td>
                                <td>{text(c.proposed)}</td>
                            </tr>
                        ))}
                </tbody>
            </table>
        </section>
    );
}
