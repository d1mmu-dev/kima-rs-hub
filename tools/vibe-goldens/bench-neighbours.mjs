// Times upstream's own nearestColumns (TypeScript, single thread, Node type stripping) on synthetic 512-d tracks,
// for comparison with crates/kima-vibe/examples/neighbour_bench.rs (Task 8, owner decision Q7).
//
//   node tools/vibe-goldens/bench-neighbours.mjs [N=20000] [ROWS=200]
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const upstream = process.env.UPSTREAM ?? path.join(os.homedir(), ".hermes/cache/scratch/kima-hub-latest");
const src = path.join(upstream, "backend/src/services/matching");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "vibe-bench-"));
for (const name of ["exactMath", "formula", "neighbours"]) {
    let text = fs.readFileSync(path.join(src, `${name}.ts`), "utf8");
    text = text.replace(/from "(\.\/[A-Za-z]+)"/g, 'from "$1.ts"');
    fs.writeFileSync(path.join(tmp, `${name}.ts`), text);
}
fs.writeFileSync(path.join(tmp, "worker.ts"), "export function runInWorker(): never { throw new Error('stub'); }\n");
const formula = await import(pathToFileURL(path.join(tmp, "formula.ts")));
const { nearestColumns } = await import(pathToFileURL(path.join(tmp, "neighbours.ts")));

const n = Number(process.argv[2] ?? 20000);
const rows = Number(process.argv[3] ?? 200);
let s = 0x2545f4914f6cdd1dn;
const next = () => {
    s ^= (s << 13n) & 0xffffffffffffffffn;
    s ^= s >> 7n;
    s ^= (s << 17n) & 0xffffffffffffffffn;
    return Number(s >> 11n) / 2 ** 53 * 2 - 1;
};
const artists = n / 10;
const centres = Array.from({ length: Math.min(artists, 2000) }, () => Array.from({ length: 512 }, next));
const tracks = Array.from({ length: n }, (_, i) => {
    const a = Math.floor(i / 10);
    const c = centres[a % centres.length];
    const f = [next(), next()];
    const fn = Math.hypot(f[0], f[1]);
    return {
        trackId: String(i).padStart(8, "0"),
        albumId: `al${a}-${Math.floor((i % 10) / 5)}`,
        artistId: `ar${String(a).padStart(6, "0")}`,
        sound: formula.unit(Float64Array.from(c, (x) => x + 0.8 * next())),
        feel: [f[0] / fn, f[1] / fn],
    };
});
const genres = ["rock", "pop", "jazz", "folk", "metal", "techno"];
const { model } = formula.buildGenreModel(
    Array.from({ length: artists }, (_, a) => ({
        artistId: `ar${String(a).padStart(6, "0")}`,
        tracks: 10,
        genres: a % 4 === 0 ? [] : [genres[a % 6], genres[Math.floor(a / 6) % 6]],
    })),
);
const t = performance.now();
const near = nearestColumns(tracks.slice(0, rows), tracks, model, 0.0625, 14, false);
const sec = (performance.now() - t) / 1000;
console.log(`node ${process.version}: ${rows} rows x ${n} cols: ${sec.toFixed(2)} s (${(rows * n / sec / 1e6).toFixed(1)} M pairs/s), max ${near.maxScore.toFixed(4)}`);
const full = (sec * n) / rows;
console.log(`=> full same-set sweep of ${n}: ${full.toFixed(0)} s; extrapolated to 100000: ${((full * (1e5 / n) ** 2) / 60).toFixed(1)} min single-thread`);
