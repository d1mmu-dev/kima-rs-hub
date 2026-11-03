import type { Capabilities, Target } from "../types";
export function TargetPicker({
    value,
    onChange,
    capabilities,
    disabled = false,
}: {
    value: Target;
    onChange: (t: Target) => void;
    capabilities: Capabilities;
    disabled?: boolean;
}) {
    return (
        <fieldset disabled={disabled}>
            <legend>Write target</legend>
            {(
                [
                    ["files", "Write files"],
                    ["copy", "Organized copy"],
                    ["db", "Database only"],
                ] as const
            ).map(([id, label]) => (
                <label key={id} className="block">
                    <input
                        type="radio"
                        name="autotag-target"
                        value={id}
                        checked={value === id}
                        disabled={!capabilities[id].available}
                        onChange={() => onChange(id)}
                    />{" "}
                    {label}
                    {!capabilities[id].available &&
                        ` — ${"reason" in capabilities[id] ? capabilities[id].reason : "Unavailable"}`}
                </label>
            ))}
        </fieldset>
    );
}
