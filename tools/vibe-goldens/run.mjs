// Regenerates crates/kima-vibe/tests/fixtures/ts-golden-*.json from upstream's pure TypeScript modules.
//
//   node tools/vibe-goldens/run.mjs [outDir]
//
// Environment: UPSTREAM = path of a kima-hub checkout at c7527577d925b20f5adf410c7fd4e0c8ac250f69
// (default ~/.hermes/cache/scratch/kima-hub-latest). Node >= 23.6 (native TypeScript type stripping; no tsx).
//
// The five pure modules are copied to a temporary directory with their relative imports given a ".ts"
// extension (Node's type stripping does not resolve extensionless specifiers) and `./worker` replaced by a
// stub (neighbours.ts imports runInWorker, which only the worker entry point uses). The single other change:
// `gaussian` in mapLayout.ts is exported so the PRNG golden can cover it. Nothing else is edited.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";

const upstream = process.env.UPSTREAM ?? path.join(os.homedir(), ".hermes/cache/scratch/kima-hub-latest");
const src = path.join(upstream, "backend/src/services/matching");
const here = path.dirname(new URL(import.meta.url).pathname);
const out = path.resolve(process.argv[2] ?? path.join(here, "../../crates/kima-vibe/tests/fixtures"));
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "vibe-goldens-"));

for (const name of ["exactMath", "formula", "mapLayout", "mapCompute", "neighbours"]) {
    let text = fs.readFileSync(path.join(src, `${name}.ts`), "utf8");
    text = text.replace(/from "(\.\/[A-Za-z]+)"/g, 'from "$1.ts"');
    if (name === "mapLayout") text = text.replace("\nfunction gaussian(", "\nexport function gaussian(");
    fs.writeFileSync(path.join(tmp, `${name}.ts`), text);
}
fs.writeFileSync(path.join(tmp, "worker.ts"), "export function runInWorker(): never { throw new Error('stub'); }\n");

const exact = await import(pathToFileURL(path.join(tmp, "exactMath.ts")));
const formula = await import(pathToFileURL(path.join(tmp, "formula.ts")));
const layoutMod = await import(pathToFileURL(path.join(tmp, "mapLayout.ts")));
const compute = await import(pathToFileURL(path.join(tmp, "mapCompute.ts")));
const neighbours = await import(pathToFileURL(path.join(tmp, "neighbours.ts")));

// ---- bit helpers -------------------------------------------------------------------------------
const f64 = new Float64Array(1);
const u64 = new BigUint64Array(f64.buffer);
const bits = (x) => ((f64[0] = x), u64[0].toString(16).padStart(16, "0"));
const fromBits = (h) => ((u64[0] = BigInt("0x" + h)), f64[0]);
const bitsOf = (arr) => Array.from(arr, bits);
const write = (name, value) => fs.writeFileSync(path.join(out, name), JSON.stringify(value) + "\n");

const provenance = {
    upstream: "https://github.com/Chevron7Locked/kima-hub",
    commit: "c7527577d925b20f5adf410c7fd4e0c8ac250f69",
    generator: "tools/vibe-goldens/run.mjs",
    note: "f64 values are IEEE-754 bit patterns, 16 lowercase hex digits",
};

// ---- exact maths + PRNG --------------------------------------------------------------------------
{
    const rng = layoutMod.seededRandom(11);
    const cases = { exp: [], log: [], atan2: [], pow: [] };
    const special = [0, -0, 1, -1, 0.5, 2, Math.E, 1e-300, 5e-324, 1e300, 709.78, 710, -745.2, -746, Infinity, -Infinity, NaN];
    for (const x of special) {
        cases.exp.push([x]);
        cases.log.push([x]);
    }
    for (let i = 0; i < 2000 - special.length; i++) {
        cases.exp.push([(rng() * 2 - 1) * (i % 7 === 0 ? 745 : 20)]);
        cases.log.push([Math.exp((rng() * 2 - 1) * (i % 5 === 0 ? 700 : 30)) * (i % 11 === 0 ? 1.0000001 : 1)]);
    }
    const atanSpecial = [[0, 1], [0, -1], [-0, -1], [1, 0], [-1, 0], [0, 0], [Infinity, Infinity], [-Infinity, Infinity], [Infinity, -Infinity], [1, Infinity], [1, -Infinity], [NaN, 1], [1, NaN], [1e300, 1e-300], [1e-300, -1e300]];
    for (const p of atanSpecial) cases.atan2.push(p);
    for (let i = 0; i < 2000 - atanSpecial.length; i++) {
        const scale = Math.exp((rng() * 2 - 1) * 20);
        cases.atan2.push([(rng() * 2 - 1) * scale, (rng() * 2 - 1) * (i % 3 === 0 ? 1 : scale)]);
    }
    for (let i = 0; i < 2000; i++) cases.pow.push([Math.exp((rng() * 2 - 1) * 10), (rng() * 2 - 1) * 8]);
    cases.pow.push([0, 2], [0, -1]);
    const run = (name, fn) => cases[name].map((a) => ({ in: a.map(bits), out: bits(fn(...a)) }));
    const prng = layoutMod.seededRandom(42);
    const prngOut = Array.from({ length: 1000 }, () => bits(prng()));
    const g = layoutMod.seededRandom(1234);
    const gaussOut = Array.from({ length: 1000 }, () => bits(layoutMod.gaussian(g)));
    write("ts-golden-exactmath.json", {
        ...provenance,
        exp: run("exp", exact.exp),
        log: run("log", exact.log),
        atan2: run("atan2", exact.atan2),
        pow: run("pow", exact.pow),
        prngSeed42: prngOut,
        gaussianSeed1234: gaussOut,
    });
}

// ---- the two libraries --------------------------------------------------------------------------
const GENRES = ["rock", "synth-pop", "darkwave", "new wave", "jazz", "ambient", "techno", "folk", "hip hop", "metal"];

function statsOf(raw) {
    const mean = [0, 1].map((k) => raw.reduce((s, r) => s + r[k], 0) / raw.length);
    const sd = [0, 1].map((k) => Math.sqrt(raw.reduce((s, r) => s + (r[k] - mean[k]) ** 2, 0) / raw.length));
    return { mean, sd };
}

function buildLibrary(rows, artistGenres) {
    // rows: {id, albumId, artistId, sound, valence, arousal}; sorted by id like the engine's ORDER BY track_id
    rows = [...rows].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const stats = statsOf(rows.map((r) => [Math.fround(r.valence), Math.fround(r.arousal)]));
    const tracks = rows.map((r) => ({
        trackId: r.id,
        albumId: r.albumId,
        artistId: r.artistId,
        sound: formula.unit(r.sound),
        feel: formula.feelVector([Math.fround(r.valence), Math.fround(r.arousal)], stats),
    }));
    const count = new Map();
    for (const t of tracks) count.set(t.artistId, (count.get(t.artistId) ?? 0) + 1);
    const { model, spread } = formula.buildGenreModel(
        Object.entries(artistGenres).map(([artistId, genres]) => ({ artistId, tracks: count.get(artistId) ?? 0, genres })),
    );
    const dim = tracks[0].sound.length;
    const meanSound = new Float64Array(dim);
    for (const t of tracks) for (let d = 0; d < dim; d++) meanSound[d] += t.sound[d] / tracks.length;
    const soundSpread = 1 - formula.dot(meanSound, meanSound);
    const wg = formula.genreWeight(soundSpread, spread);
    const feelZ = new Float64Array(tracks.length * 2);
    rows.forEach((r, i) => {
        feelZ[i * 2] = stats.sd[0] > 0 ? (Math.fround(r.valence) - stats.mean[0]) / stats.sd[0] : 0;
        feelZ[i * 2 + 1] = stats.sd[1] > 0 ? (Math.fround(r.arousal) - stats.mean[1]) / stats.sd[1] : 0;
    });
    return { rows, stats, tracks, model, spread, soundSpread, wg, feelZ };
}

const parity = JSON.parse(fs.readFileSync(path.join(src, "__tests__/fixtures/parity.json"), "utf8"));
const mb = JSON.parse(fs.readFileSync(path.join(src, "mbGenres.json"), "utf8"));
const key = (s) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
const MB = new Map();
for (const n of mb.genres) if (!MB.has(key(n))) MB.set(key(n), n.toLowerCase());
const mbFilter = (list) => [...new Set(list.map((g) => MB.get(key(g))).filter(Boolean))];

function synthetic300() {
    const rng = layoutMod.seededRandom(7);
    const dim = 32;
    const artists = Array.from({ length: 12 }, (_, i) => `art${String(i).padStart(2, "0")}`);
    const centre = artists.map(() => Array.from({ length: dim }, () => rng() * 2 - 1));
    const artistGenres = {};
    artists.forEach((a, i) => {
        const n = i % 4 === 3 ? 0 : 1 + Math.floor(rng() * 3);
        const picked = new Set();
        while (picked.size < n) picked.add(GENRES[Math.floor(rng() * GENRES.length)]);
        artistGenres[a] = mbFilter([...picked]);
    });
    const rows = [];
    for (let i = 0; i < 300; i++) {
        const a = Math.floor(rng() * artists.length);
        const album = `alb${String(a).padStart(2, "0")}${Math.floor(rng() * 3)}`;
        const sound = centre[a].map((c) => c + (rng() * 2 - 1) * 0.8);
        rows.push({ id: `t${String(i).padStart(3, "0")}`, albumId: album, artistId: artists[a], sound, valence: rng(), arousal: rng() });
    }
    return { rows, artistGenres };
}

function libraryJson(name, lib, extra) {
    return {
        ...provenance,
        library: name,
        tracks: lib.rows.map((r) => ({ id: r.id, albumId: r.albumId, artistId: r.artistId, sound: Array.from(r.sound), valence: r.valence, arousal: r.arousal })),
        artistGenres: extra.artistGenres,
        soundSpread: bits(lib.soundSpread),
        genreSpread: bits(lib.spread),
        genreWeight: bits(lib.wg),
        feelMean: lib.stats.mean.map(bits),
        feelSd: lib.stats.sd.map(bits),
    };
}

const libs = {
    fixture50: (() => {
        const artistGenres = {};
        for (const a of parity.artists) artistGenres[a.artistId] = mbFilter(a.genres);
        return { rows: parity.tracks, artistGenres };
    })(),
    synthetic300: synthetic300(),
};

const goldenLibraries = {};
const neighbourGolden = { ...provenance, libraries: {} };
const layoutGolden = { ...provenance, libraries: {} };
for (const [name, { rows, artistGenres }] of Object.entries(libs)) {
    const lib = buildLibrary(rows, artistGenres);
    goldenLibraries[name] = libraryJson(name, lib, { artistGenres });
    const wgSq = lib.wg ** 2;
    const near = neighbours.nearestColumns(lib.tracks, lib.tracks, lib.model, wgSq, layoutMod.MAP_NEIGHBOURS - 1, true);
    neighbourGolden.libraries[name] = {
        k: near.k,
        idx: Array.from(near.idx),
        score: bitsOf(near.score),
        maxScore: bits(near.maxScore),
        capped10: Object.fromEntries(
            [...neighbours.neighbourLists(lib.tracks, lib.model, wgSq, 10)].map(([id, list]) => [id, list.map(([tid, s]) => [tid, bits(s)])]),
        ),
    };
    const laid = compute.layoutLibrary(lib.tracks, lib.feelZ, lib.model, lib.wg);
    layoutGolden.libraries[name] = {
        positions: bitsOf(laid.positions),
        colour: bitsOf(laid.colour),
        maxScore: bits(laid.maxScore),
        frame: {
            centre: laid.frame.centre.map(bits),
            scale: bits(laid.frame.scale),
            turn: laid.frame.turn.map(bits),
            hueMin: bits(laid.frame.hueMin),
            hueMax: bits(laid.frame.hueMax),
        },
    };
}
write("ts-golden-library.json", goldenLibraries);
write("ts-golden-neighbours.json", neighbourGolden);
write("ts-golden-layout.json", layoutGolden);

// ---- placement: lay out the first 250 tracks of synthetic300, place the other 50 ------------------------
{
    const { rows, artistGenres } = libs.synthetic300;
    const all = buildLibrary(rows, artistGenres);
    const first = all.rows.filter((_, i) => i < 250);
    const rest = all.rows.filter((_, i) => i >= 250);
    // The engine builds both from one state (library-wide stats); only the tracks differ.
    const laidTracks = all.tracks.slice(0, 250);
    const newTracks = all.tracks.slice(250);
    const laid = compute.layoutLibrary(laidTracks, all.feelZ.slice(0, 500), all.model, all.wg);
    const placed = compute.placeTracks(laidTracks, laid.positions, laid.positions, newTracks, all.model, all.wg, laid.frame, laid.maxScore);
    write("ts-golden-place.json", {
        ...provenance,
        library: "synthetic300",
        laidOutCount: first.length,
        newCount: rest.length,
        laidOutPositions: bitsOf(laid.positions),
        frame: {
            centre: laid.frame.centre.map(bits),
            scale: bits(laid.frame.scale),
            turn: laid.frame.turn.map(bits),
            hueMin: bits(laid.frame.hueMin),
            hueMax: bits(laid.frame.hueMax),
        },
        maxScore: bits(laid.maxScore),
        positions: bitsOf(placed.positions),
        colour: bitsOf(placed.colour),
    });
}
fs.rmSync(tmp, { recursive: true, force: true });
void fromBits;
console.log("goldens written to", out);
