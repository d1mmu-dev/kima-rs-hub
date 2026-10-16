import { BRAND_GRADIENT_CSS } from "@/lib/brandGradient";

/**
 * The Kima mark: the waveform logo filled with the brand gradient, amber at the top through rose and violet
 * to azure, its hue swaying ±14° over 16 s like the mobile app's drift mark. Still under reduced motion.
 */
export function KimaMark({ size, className = "" }: { size: number; className?: string }) {
    return (
        <span
            role="img"
            aria-label="Kima"
            className={`kima-mark inline-block shrink-0 ${className}`}
            style={{ width: size, height: size, background: BRAND_GRADIENT_CSS }}
        />
    );
}
