import * as THREE from "three";

/**
 * Picks the star that projects nearest the pointer. Stars are projected once per
 * camera pose, on the first pointer event after it moves, into a pixel grid, so
 * a hover is a few lookups even with 100,000 stars.
 */
const CELL = 32;

export class ScreenPicker {
    private sx = new Float32Array(0);
    private sy = new Float32Array(0);
    private depth = new Float32Array(0);
    private cells = new Map<number, number[]>();
    private cols = 1;
    private poseKey = "";
    private readonly v = new THREE.Vector3();

    constructor(private readonly positions: Float32Array) {}

    private refresh(camera: THREE.Camera, width: number, height: number) {
        camera.updateMatrixWorld();
        const e = camera.matrixWorld.elements;
        const p = (camera as THREE.PerspectiveCamera).projectionMatrix.elements;
        const key = `${width}x${height}|${e.map((v) => v.toFixed(4)).join(",")}|${p[0].toFixed(4)},${p[5].toFixed(4)}`;
        if (key === this.poseKey) return;
        this.poseKey = key;

        const n = this.positions.length / 3;
        if (this.sx.length !== n) {
            this.sx = new Float32Array(n);
            this.sy = new Float32Array(n);
            this.depth = new Float32Array(n);
        }
        this.cols = Math.ceil(width / CELL) + 1;
        this.cells.clear();
        for (let i = 0; i < n; i++) {
            this.v.set(this.positions[i * 3], this.positions[i * 3 + 1], this.positions[i * 3 + 2]).project(camera);
            if (this.v.z < -1 || this.v.z > 1) { this.sx[i] = NaN; continue; } // behind the camera or past far plane
            const x = ((this.v.x + 1) / 2) * width;
            const y = ((1 - this.v.y) / 2) * height;
            this.sx[i] = x;
            this.sy[i] = y;
            this.depth[i] = this.v.z;
            if (x < -CELL || y < -CELL || x > width + CELL || y > height + CELL) continue;
            const k = Math.floor((y + CELL) / CELL) * this.cols + Math.floor((x + CELL) / CELL);
            const bucket = this.cells.get(k);
            if (bucket) bucket.push(i);
            else this.cells.set(k, [i]);
        }
    }

    /** Index of the star drawn nearest to (x, y) in canvas pixels, within `radius` px; nearer-to-camera wins ties. */
    pick(camera: THREE.Camera, width: number, height: number, x: number, y: number, radius: number): number | null {
        this.refresh(camera, width, height);
        const c0 = Math.floor((x - radius + CELL) / CELL), c1 = Math.floor((x + radius + CELL) / CELL);
        const r0 = Math.floor((y - radius + CELL) / CELL), r1 = Math.floor((y + radius + CELL) / CELL);
        let best: number | null = null;
        let bestScore = Infinity;
        for (let r = r0; r <= r1; r++) {
            for (let c = c0; c <= c1; c++) {
                const bucket = this.cells.get(r * this.cols + c);
                if (!bucket) continue;
                for (const i of bucket) {
                    const d = Math.hypot(this.sx[i] - x, this.sy[i] - y);
                    if (d > radius) continue;
                    const score = d + this.depth[i] * 2; // a hair of preference for the nearer star
                    if (score < bestScore) { bestScore = score; best = i; }
                }
            }
        }
        return best;
    }

    /** Screen position of star `i` for the current pose (NaN when off-camera). */
    screenOf(camera: THREE.Camera, width: number, height: number, i: number): [number, number] {
        this.refresh(camera, width, height);
        return [this.sx[i], this.sy[i]];
    }
}
