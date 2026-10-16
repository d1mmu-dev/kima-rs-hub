import * as THREE from "three";
import { moodIndex } from "../mapUtils";
import type { MapTrack } from "../types";

/** Half-extent of the map in world units: normalised 0..1 coordinates span ±FIELD_RADIUS. */
export const FIELD_RADIUS = 60;

/**
 * Each track at the position the similarity step gave it, normalised to a unit
 * cube, so tracks the engine matches sit together. World axes: x → x, z
 * (height) → y, y → depth.
 */
export function toWorld(track: Pick<MapTrack, "x" | "y" | "z">): [number, number, number] {
    const s = FIELD_RADIUS * 2;
    return [(track.x - 0.5) * s, (track.z - 0.5) * s, (track.y - 0.5) * s];
}

export interface TrackField {
    geometry: THREE.BufferGeometry;
    /** World position per track index, xyz interleaved. */
    positions: Float32Array;
    /** Emphasis the scene eases each star toward (see starShader aEmph). */
    emphasis: THREE.BufferAttribute;
    indexById: Map<string, number>;
    /** Centre of the cloud and the radius holding 90% of its stars. */
    center: THREE.Vector3;
    radius: number;
    /** World-space star size used for rings and offsets. */
    baseSize: number;
    /** Seconds until the last star has finished its load-in. */
    revealDuration: number;
}

// Deterministic per-track noise, so a track twinkles the same way every visit.
function hash01(i: number, salt: number) {
    let h = (i * 2654435761) ^ (salt * 40503);
    h = Math.imul(h ^ (h >>> 15), 2246822519);
    h = Math.imul(h ^ (h >>> 13), 3266489917);
    return ((h ^ (h >>> 16)) >>> 0) / 4294967296;
}

// Load-in: the cloud lights up in angular sectors around its centre, one after
// another; within a sector the core ignites before the edges, with a little
// per-star jitter so it reads as sparkle rather than a wipe.
const REVEAL_SECTORS = 8;
const REVEAL_SECTOR_GAP = 0.15;
const REVEAL_WITHIN = 0.22;
const REVEAL_JITTER = 0.16;
const REVEAL_FADE = 0.3;

const STAR_WHITE = new THREE.Color("#f3ecdd");

export function buildTrackField(tracks: MapTrack[]): TrackField {
    const n = tracks.length;
    const positions = new Float32Array(n * 3);
    const colors = new Float32Array(n * 3);
    const sizes = new Float32Array(n);
    const phases = new Float32Array(n);
    const twinkle = new Float32Array(n);
    const spark = new Float32Array(n);
    const mood = new Float32Array(n);
    const reveal = new Float32Array(n);
    const emph = new Float32Array(n).fill(1);
    const indexById = new Map<string, number>();

    // Scales the rings and the closest zoom to the library's size.
    const baseSize = THREE.MathUtils.clamp(5 / Math.pow(Math.max(n, 1), 0.12), 0.7, 2.4);
    const c = new THREE.Color();
    const center = new THREE.Vector3();

    for (let i = 0; i < n; i++) {
        const t = tracks[i];
        indexById.set(t.id, i);
        const [wx, wy, wz] = toWorld(t);
        positions[i * 3] = wx;
        positions[i * 3 + 1] = wy;
        positions[i * 3 + 2] = wz;
        center.x += wx; center.y += wy; center.z += wz;

        // Starlight: a bright few over a faint many.
        c.copy(STAR_WHITE);
        const notable = hash01(i, 7) < 0.11;
        const brightness = notable ? 0.7 + hash01(i, 4) * 0.3 : 0.16 + hash01(i, 4) * 0.24;
        colors[i * 3] = c.r * brightness;
        colors[i * 3 + 1] = c.g * brightness;
        colors[i * 3 + 2] = c.b * brightness;

        sizes[i] = notable ? 1.5 + hash01(i, 6) * 0.8 : 0.7 + hash01(i, 6) * 0.4;
        phases[i] = hash01(i, 1) * Math.PI * 2;
        twinkle[i] = 0.14 + hash01(i, 2) * 0.2;
        spark[i] = hash01(i, 3) < 0.02 ? 1 : 0;
        mood[i] = moodIndex(t.dominantMood);
    }
    if (n > 0) center.divideScalar(n);

    // Radius holding 90% of the stars, so a few outliers don't set the framing.
    const dists = new Float32Array(n);
    for (let i = 0; i < n; i++) {
        dists[i] = Math.hypot(positions[i * 3] - center.x, positions[i * 3 + 1] - center.y, positions[i * 3 + 2] - center.z);
    }
    const sorted = Float32Array.from(dists).sort();
    const radius = Math.max(8, n > 0 ? sorted[Math.floor((n - 1) * 0.9)] : FIELD_RADIUS);
    const maxDist = n > 0 ? sorted[n - 1] || 1 : 1;

    let revealDuration = 0;
    for (let i = 0; i < n; i++) {
        const angle = Math.atan2(positions[i * 3 + 2] - center.z, positions[i * 3] - center.x);
        const sector = Math.min(REVEAL_SECTORS - 1, Math.floor(((angle + Math.PI) / (Math.PI * 2)) * REVEAL_SECTORS));
        const delay = sector * REVEAL_SECTOR_GAP
            + (dists[i] / maxDist) * REVEAL_WITHIN
            + (hash01(i, 5) - 0.5) * REVEAL_JITTER;
        reveal[i] = Math.max(0, delay);
        revealDuration = Math.max(revealDuration, reveal[i] + REVEAL_FADE);
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3));
    geometry.setAttribute("aColor", new THREE.BufferAttribute(colors, 3));
    geometry.setAttribute("aSize", new THREE.BufferAttribute(sizes, 1));
    geometry.setAttribute("aPhase", new THREE.BufferAttribute(phases, 1));
    geometry.setAttribute("aTwAmp", new THREE.BufferAttribute(twinkle, 1));
    geometry.setAttribute("aSpark", new THREE.BufferAttribute(spark, 1));
    geometry.setAttribute("aMood", new THREE.BufferAttribute(mood, 1));
    geometry.setAttribute("aReveal", new THREE.BufferAttribute(reveal, 1));
    const emphasis = new THREE.BufferAttribute(emph, 1);
    emphasis.setUsage(THREE.DynamicDrawUsage);
    geometry.setAttribute("aEmph", emphasis);
    geometry.computeBoundingSphere();

    return { geometry, positions, emphasis, indexById, center, radius, baseSize, revealDuration };
}

/**
 * Ease every star's emphasis toward its goal. Returns true once all have
 * arrived, so the caller can stop uploading the attribute.
 */
export function easeEmphasis(field: TrackField, goal: Float32Array, step: number): boolean {
    const current = field.emphasis.array as Float32Array;
    let settled = true;
    for (let i = 0; i < current.length; i++) {
        const d = goal[i] - current[i];
        if (Math.abs(d) > 0.003) { current[i] += d * step; settled = false; } else current[i] = goal[i];
    }
    field.emphasis.needsUpdate = true;
    return settled;
}
