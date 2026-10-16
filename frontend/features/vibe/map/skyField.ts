import * as THREE from "three";

/**
 * Deep-space backdrop: soft twinkling stars on a flattened shell
 * around the map, with a few faint far clumps for depth. Decoration, not data.
 */
const SKY_STARS = 8000;

// Mostly warm white, some gold and warm, a little rose, one faint purple.
const PALETTE = ["#f3ecdd", "#f3ecdd", "#efe6d2", "#f0c07a", "#e6b892", "#e79db0", "#b06fc8"];
const CLUMPS = [new THREE.Vector3(-270, 66, -170), new THREE.Vector3(300, -92, 150), new THREE.Vector3(120, 150, -320)];

function rng(seed: number) {
    let a = seed >>> 0;
    return () => {
        a = (a + 0x6d2b79f5) | 0;
        let t = Math.imul(a ^ (a >>> 15), 1 | a);
        t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
}

export function buildSkyField(): THREE.BufferGeometry {
    const rand = rng(0x9e3d);
    const pos = new Float32Array(SKY_STARS * 3);
    const col = new Float32Array(SKY_STARS * 3);
    const size = new Float32Array(SKY_STARS);
    const phase = new Float32Array(SKY_STARS);
    const amp = new Float32Array(SKY_STARS);
    const spark = new Float32Array(SKY_STARS);
    const palette = PALETTE.map((hex) => new THREE.Color(hex));

    for (let i = 0; i < SKY_STARS; i++) {
        if (i % 11 === 0) {
            const k = CLUMPS[i % CLUMPS.length];
            pos[i * 3] = k.x + (rand() - 0.5) * 74;
            pos[i * 3 + 1] = k.y + (rand() - 0.5) * 26;
            pos[i * 3 + 2] = k.z + (rand() - 0.5) * 74;
        } else {
            const u = rand() * 2 - 1;
            const th = rand() * Math.PI * 2;
            const s = Math.sqrt(1 - u * u);
            const r = 140 + rand() * 270;
            pos[i * 3] = Math.cos(th) * s * r;
            pos[i * 3 + 1] = u * r * 0.6;
            pos[i * 3 + 2] = Math.sin(th) * s * r;
        }
        const c = palette[Math.floor(rand() * palette.length)];
        // A bright few over a faint many.
        const bright = rand() < 0.06;
        const b = bright ? 0.9 + rand() * 0.5 : 0.12 + Math.pow(rand(), 2.4) * 0.5;
        col[i * 3] = c.r * b;
        col[i * 3 + 1] = c.g * b;
        col[i * 3 + 2] = c.b * b;
        size[i] = bright ? 1.4 + rand() * 1.0 : 0.7 + rand() * 0.6;
        phase[i] = rand() * Math.PI * 2;
        amp[i] = 0.4 + rand() * 0.45;
        spark[i] = rand() < 0.04 ? 1 : 0;
    }

    const g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.BufferAttribute(pos, 3));
    g.setAttribute("aColor", new THREE.BufferAttribute(col, 3));
    g.setAttribute("aSize", new THREE.BufferAttribute(size, 1));
    g.setAttribute("aPhase", new THREE.BufferAttribute(phase, 1));
    g.setAttribute("aTwAmp", new THREE.BufferAttribute(amp, 1));
    g.setAttribute("aSpark", new THREE.BufferAttribute(spark, 1));
    g.setAttribute("aEmph", new THREE.BufferAttribute(new Float32Array(SKY_STARS).fill(1), 1));
    g.setAttribute("aMood", new THREE.BufferAttribute(new Float32Array(SKY_STARS).fill(-1), 1));
    g.setAttribute("aReveal", new THREE.BufferAttribute(new Float32Array(SKY_STARS), 1));
    return g;
}
