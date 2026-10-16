import type { ColorPalette } from "@/hooks/useImageColor";

export function ColorWash({ colors }: { colors: ColorPalette | null }) {
    return (
        <div
            className="absolute inset-x-0 top-0 pointer-events-none"
            style={{
                height: "25vh",
                background: colors
                    ? `linear-gradient(to bottom, ${colors.vibrant}10 0%, ${colors.vibrant}05 40%, transparent 100%)`
                    : "transparent",
            }}
        />
    );
}
