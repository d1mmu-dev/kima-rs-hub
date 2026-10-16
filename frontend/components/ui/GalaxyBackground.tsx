"use client";

/** A fixed gradient behind the page, tinted by `primaryColor` when given. */

interface GalaxyBackgroundProps {
    /** Background tint color. */
    primaryColor?: string;
}

export function GalaxyBackground({ primaryColor }: GalaxyBackgroundProps = {}) {
    const hexToRgb = (hex: string) => {
        const result = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(hex);
        return result ? {
            r: parseInt(result[1], 16),
            g: parseInt(result[2], 16),
            b: parseInt(result[3], 16)
        } : null;
    };

    const baseColor = primaryColor ? hexToRgb(primaryColor) : null;

    return (
        <div
            className="fixed inset-0 pointer-events-none z-0 overflow-hidden"
            style={{
                willChange: 'auto',
                transform: 'translateZ(0)',
            }}
        >
            {baseColor ? (
                <>
                    <div
                        className="absolute inset-0 bg-gradient-to-t to-transparent"
                        style={{
                            backgroundImage: `linear-gradient(to top, rgba(${baseColor.r}, ${baseColor.g}, ${baseColor.b}, 0.15), rgba(${baseColor.r}, ${baseColor.g}, ${baseColor.b}, 0.05), transparent)`
                        }}
                    />
                    <div className="absolute inset-0 bg-gradient-to-t from-black/40 via-black/10 to-transparent" />
                </>
            ) : (
                <>
                    <div className="absolute inset-0 bg-gradient-to-t from-purple-950/15 via-purple-950/5 to-transparent" />
                    <div className="absolute inset-0 bg-gradient-to-t from-black/40 via-black/10 to-transparent" />
                </>
            )}
        </div>
    );
}
