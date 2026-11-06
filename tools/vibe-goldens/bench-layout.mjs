// Times upstream's own layoutLibrary (TypeScript, single thread, Node type stripping) stage by stage on the synthetic
// 512-d library of crates/kima-vibe/examples/layout_bench.rs (same generator, same draw order), for the Q7 comparison.
//
//   node tools/vibe-goldens/bench-layout.mjs [N=10000]
//
// `centredRows` (the PCA start) is not exported upstream, so the PCA start is the whole `layoutLibrary` time minus the
// stages measured here; the layout stage is fed a random start (its cost does not depend on the start's values).
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const upstream = process.env.UPSTREAM ?? path.join(os.homedir(), ".hermes/cache/scratch/kima-hub-latest");
const src = path.join(upstream, "backend/src/services/matching");
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "vibe-bench-"));
for (const name of ["exactMath", "formula", "mapLayout", "mapCompute", "neighbours"]) {
    let text = fs.readFileSync(path.join(src, `${name}.ts`), "utf8");
    text = text.replace(/from "(\.\/[A-Za-z]+)"/g, 'from "$1.ts"');
    fs.writeFileSync(path.join(tmp, `${name}.ts`), text);
}
fs.writeFileSync(path.join(tmp, "worker.ts"), "export function runInWorker(): never { throw new Error('stub'); }\n");
const imp = (n) => import(pathToFileURL(path.join(tmp, `${n}.ts`)));
const formula = await imp("formula");
const layoutMod = await imp("mapLayout");
const compute = await imp("mapCompute");
const { nearestColumns } = await imp("neighbours");

const n = Number(process.argv[2] ?? 10000);
let s = 0x2545f4914f6cdd1dn;
const next = () => {
    s ^= (s << 13n) & 0xffffffffffffffffn;
    s ^= s >> 7n;
    s ^= (s << 17n) & 0xffffffffffffffffn;
    return Number(s >> 11n) / 2 ** 53 * 2 - 1;
};
const artists = n / 10;
const centres = Array.from({ length: Math.min(artists, 2000) }, () => Array.from({ length: 512 }, next));
const feelZ = new Float64Array(n * 2);
const tracks = Array.from({ length: n }, (_, i) => {
    const a = Math.floor(i / 10);
    const c = centres[a % centres.length];
    const f = [next(), next()];
    feelZ[i * 2] = f[0];
    feelZ[i * 2 + 1] = f[1];
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
const gw = 0.25;
const time = (label, fn) => {
    const t = performance.now();
    const r = fn();
    console.log(`${label.padEnd(28)} ${((performance.now() - t) / 1000).toFixed(1).padStart(8)} s`);
    return r;
};
console.log(`node ${process.version} n=${n} epochs=${layoutMod.layoutEpochs(n)}`);
const near = time("1 neighbour sweep", () => nearestColumns(tracks, tracks, model, gw * gw, 14, true));
const k = near.k + 1;
const graph = { rows: n, k, idx: new Int32Array(n * k), dist: new Float64Array(n * k) };
for (let i = 0; i < n; i++) {
    graph.idx[i * k] = i;
    for (let r = 0; r < near.k; r++) {
        graph.idx[i * k + 1 + r] = near.idx[i * near.k + r];
        graph.dist[i * k + 1 + r] = near.maxScore - near.score[i * near.k + r];
    }
}
const edges = time("3 fuzzy graph", () => layoutMod.fuzzyGraph(graph, layoutMod.layoutEpochs(n)));
console.log(`   ${edges.head.length} edges`);
const start = Float64Array.from({ length: n * 3 }, () => (next() + 1) * 5);
const pos = time("4 layout (incl. fuzzy graph)", () => layoutMod.layout(graph, start));
time("5 colour field", () => layoutMod.colourField(pos, n, feelZ));
time("1-5 layoutLibrary (total)", () => compute.layoutLibrary(tracks, feelZ, model, gw));
console.log(`peak RSS ${(process.resourceUsage().maxRSS / 1024).toFixed(0)} MB`);
