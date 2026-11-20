// Regenerates crates/kima-mixes/tests/fixtures/draw_goldens.json from upstream's pure mix helpers.
//
//   node tools/mix-goldens/upstream.mjs [outFile]
//
// Environment: UPSTREAM = path of a kima-hub git checkout that CONTAINS the pin (default KIMA_HUB_REFERENCE,
// environment or private tools config). Every module is read from the pin's git objects
// (../vibe-goldens/upstream.mjs `readPinned`), never from a working tree. Node >= 23.6 (native type stripping).
//
// Modules (backend/src/...): services/mixes/{helpers,artistCap,mixDay,stationOrder}.ts, utils/artistSpread.ts and what
// they import (utils/{shuffle,libraryEligibility,artistNormalization}.ts, services/{artistIdentity,pgTextRules}.ts),
// copied to a temporary directory with relative imports pointed at the copies (".ts" added). Stubbed: `@prisma/client`
// (only `Prisma.sql`/`Prisma.join`, which the pure functions never call), `utils/db` (no database), and `utils/redis`
// and `utils/logger` for stationOrder.ts (only its pure `replayPasses` and constants are called). The daily pick of
// services/mixes/index.ts is taken from that file's text (the `selectedIndices` loop and the generator count) and
// run as written. The single other edit: `FreePlaces`'s constructor parameter property (`constructor(private n:
// number)`), which type stripping cannot run, becomes a field assigned in the constructor. Nothing else is edited.
// The output must be byte-identical on every run.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { PIN, readPinned } from "../vibe-goldens/upstream.mjs";

const here = path.dirname(new URL(import.meta.url).pathname);
const out = path.resolve(process.argv[2] ?? path.join(here, "../../crates/kima-mixes/tests/fixtures/draw_goldens.json"));
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "mix-goldens-"));

const files = {
    "helpers.ts": "backend/src/services/mixes/helpers.ts",
    "artistCap.ts": "backend/src/services/mixes/artistCap.ts",
    "mixDay.ts": "backend/src/services/mixes/mixDay.ts",
    "artistSpread.ts": "backend/src/utils/artistSpread.ts",
    "stationOrder.ts": "backend/src/services/mixes/stationOrder.ts",
    "shuffle.ts": "backend/src/utils/shuffle.ts",
    "libraryEligibility.ts": "backend/src/utils/libraryEligibility.ts",
    "artistNormalization.ts": "backend/src/utils/artistNormalization.ts",
    "artistIdentity.ts": "backend/src/services/artistIdentity.ts",
    "pgTextRules.ts": "backend/src/services/pgTextRules.ts",
};
const imports = {
    "@prisma/client": "./stub_prisma.ts",
    "../../utils/db": "./stub_db.ts",
    "../../utils/shuffle": "./shuffle.ts",
    "../../utils/libraryEligibility": "./libraryEligibility.ts",
    "../../utils/artistNormalization": "./artistNormalization.ts",
    "../utils/artistNormalization": "./artistNormalization.ts",
    "../artistIdentity": "./artistIdentity.ts",
    "../../utils/redis": "./stub_redis.ts",
    "../../utils/logger": "./stub_logger.ts",
    "../../utils/artistSpread": "./artistSpread.ts",
    "./artistCap": "./artistCap.ts",
    "./pgTextRules": "./pgTextRules.ts",
};
for (const [name, rel] of Object.entries(files)) {
    let text = readPinned(rel);
    text = text.replace(/from "([^"]+)"/g, (whole, spec) => {
        if (spec === "crypto") return whole;
        if (!(spec in imports)) throw new Error(`${rel}: unexpected import ${spec}`);
        return `from "${imports[spec]}"`;
    });
    if (name === "artistSpread.ts") {
        const before = "    constructor(private n: number) {\n";
        if (text.split(before).length !== 2) throw new Error("FreePlaces constructor not found");
        text = text.replace(before, "    private n: number;\n\n    constructor(n: number) {\n        this.n = n;\n");
    }
    fs.writeFileSync(path.join(tmp, name), text);
}
fs.writeFileSync(
    path.join(tmp, "stub_prisma.ts"),
    "export const Prisma = { sql: () => { throw new Error('stub'); }, join: () => { throw new Error('stub'); } };\n",
);
fs.writeFileSync(path.join(tmp, "stub_db.ts"), "export const prisma = {};\n");
fs.writeFileSync(path.join(tmp, "stub_redis.ts"), "export const redisClient = {};\n");
fs.writeFileSync(
    path.join(tmp, "stub_logger.ts"),
    "export const logger = { warn: () => {}, debug: () => {}, info: () => {}, error: () => {} };\n",
);

const helpers = await import(pathToFileURL(path.join(tmp, "helpers.ts")));
const artistCap = await import(pathToFileURL(path.join(tmp, "artistCap.ts")));
const mixDay = await import(pathToFileURL(path.join(tmp, "mixDay.ts")));
const spread = await import(pathToFileURL(path.join(tmp, "artistSpread.ts")));
const normalization = await import(pathToFileURL(path.join(tmp, "artistNormalization.ts")));
const station = await import(pathToFileURL(path.join(tmp, "stationOrder.ts")));

// ---- the daily pick, from services/mixes/index.ts as written ------------------------------------------------------
const index = readPinned("backend/src/services/mixes/index.ts");
const generatorList = /const mixGenerators = \[\n([\s\S]*?)\n {8}\];\n/.exec(index);
if (!generatorList) throw new Error("mixGenerators not found in index.ts");
const generatorCount = (generatorList[1].match(/\bfn: \(\) =>\s+generate[A-Za-z]+\(/g) ?? []).length;
if (generatorCount !== (generatorList[1].match(/\bfn:/g) ?? []).length) throw new Error("unexpected generator entry");
const dailyCount = Number(/private readonly DAILY_MIX_COUNT = (\d+);/.exec(index)[1]);
const loop = /const selectedIndices: number\[\] = \[\];\n\s+let seed = dateSeed;\n\n(\s+while \(selectedIndices\.length < this\.DAILY_MIX_COUNT\) \{[\s\S]*?\n\s+\}\n)/.exec(index);
if (!loop) throw new Error("daily pick loop not found in index.ts");
const dailyPick = new Function(
    "dateSeed",
    "mixGenerators",
    `const self = { DAILY_MIX_COUNT: ${dailyCount} };\nconst selectedIndices = [];\nlet seed = dateSeed;\n` +
        loop[1].replace("this.DAILY_MIX_COUNT", "self.DAILY_MIX_COUNT") +
        "return selectedIndices;",
);

// ---- inputs ------------------------------------------------------------------------------------------------------
const uuid = (i) => `0190${String(i).padStart(4, "0")}-7a1b-7c2d-8e3f-${String(i * 7919).padStart(12, "0")}`;
const seedStrings = [
    "",
    "a",
    "2026-11-19-" + uuid(1),
    "2026-11-19-" + uuid(2),
    "2026-01-01-" + uuid(3),
    "zażółć gęślą jaźń",
    "🎵 night drive 🌙",
    "x".repeat(1000),
    "The quick brown fox jumps over the lazy dog",
    "\u0000\uffff",
];
const ids = Array.from({ length: 60 }, (_, i) => uuid(i));

const goldens = {
    provenance: {
        upstream: "https://github.com/Chevron7Locked/kima-hub",
        commit: PIN,
        generator: "tools/mix-goldens/upstream.mjs",
    },
    seededRandom: seedStrings.map((s) => [s, helpers.getSeededRandom(s)]),
    dailyPick: {
        generators: generatorCount,
        count: dailyCount,
        cases: seedStrings.map((s) => {
            const dateSeed = helpers.getSeededRandom(s);
            return [dateSeed, dailyPick(dateSeed, new Array(generatorCount))];
        }),
    },
    seededKey: [
        [42, "t1"],
        ["42", "t1"],
        ["2026-11-19", uuid(5)],
        [987654321, "zażółć"],
        ["era-1990-2026-11-19", ""],
    ].map(([seed, id]) => [String(seed), id, helpers.seededKey(seed, id)]),
    seededOrder: [1, 42, 987654321, "2026-11-19-genre"].map((seed) => ({
        seed: String(seed),
        ids,
        order: helpers.seededOrder(ids, seed, (id) => id),
        take: [1, 2, 20, 59, 60, 80].map((take) => [take, helpers.seededTake(ids, seed, (id) => id, take)]),
    })),
};

// weightedOrder: the weights as strings so NaN and the infinities survive JSON.
const weightItems = [
    ["heavy", 9],
    ["mid", 3],
    ["light", 1],
    ["tiny", 1e-9],
    ["zero", 0],
    ["negative", -2],
    ["nan", NaN],
    ["inf", Infinity],
    ...ids.slice(0, 30).map((id, i) => [id, (i % 7) + 0.5]),
];
goldens.weightedOrder = ["seed", "0", "19999", "2026-11-19:" + uuid(9), "s29"].map((seed) => ({
    seed,
    items: weightItems.map(([id, w]) => [id, String(w)]),
    order: helpers
        .weightedOrder(weightItems, ([, w]) => w, { seed, idOf: ([id]) => id })
        .map(([id]) => id),
}));

// capByArtist over orders with crowded artists.
const capCases = [];
for (const [n, artists, limit] of [
    [30, 3, 20],
    [30, 10, 20],
    [12, 2, 20],
    [40, 5, 15],
    [9, 9, 5],
    [6, 1, 20],
    [25, 4, 8],
]) {
    const ordered = Array.from({ length: n }, (_, i) => ({ id: `t${i}`, artistId: `a${(i * 7 + (i >> 2)) % artists}` }));
    capCases.push({ limit, ordered: ordered.map((t) => [t.id, t.artistId]), picked: artistCap.capByArtist(ordered, limit) });
}
goldens.capByArtist = {
    minMixTracks: artistCap.MIN_MIX_TRACKS,
    cases: capCases,
};
goldens.songArtistKey = {
    noOneNames: [normalization.VARIOUS_ARTISTS_CANONICAL, normalization.UNKNOWN_ARTIST],
    noOneKeys: artistCap.noOneKeys(),
    cases: [
        ["t1", "ar1", artistCap.noOneKeys()[0]],
        ["t2", "ar2", artistCap.noOneKeys()[1]],
        ["t3", "ar3", "beatles"],
    ].map(([t, a, k]) => [t, a, k, artistCap.songArtistKey(t, a, k)]),
};

// randomFromSeed: the first numbers of a few streams.
goldens.randomFromSeed = ["", "station:wildcard:p1", "2026-11-19", "🎵"].map((seed) => {
    const random = spread.randomFromSeed(seed);
    return [seed, Array.from({ length: 12 }, () => random())];
});

// spreadByArtist with a seeded random, over pools of several shapes, with and without `follows`.
const pool = (counts) => counts.flatMap((c, a) => Array.from({ length: c }, (_, k) => `ar${a}#${k}`));
const artistOf = (item) => item.split("#")[0];
const spreadCases = [];
const shapes = [
    [1],
    [1, 1],
    [2, 1],
    [3, 3, 2, 1],
    [5, 1, 1],
    [10, 2],
    [4, 4, 4, 4],
    [6, 5, 1, 1, 1],
    [20, 1, 1, 1],
    Array.from({ length: 25 }, (_, i) => (i * 13) % 9 + 1),
    Array.from({ length: 60 }, (_, i) => (i % 5 === 0 ? 7 : 1 + (i % 3))),
];
shapes.forEach((counts, s) => {
    const items = pool(counts);
    for (const seed of [`spread-${s}-a`, `spread-${s}-b`]) {
        spreadCases.push({ seed, items, follows: null, order: spread.spreadByArtist(items, artistOf, spread.randomFromSeed(seed)) });
    }
    if (counts.length > 1) {
        const previousArtist = "ar0";
        spreadCases.push({
            seed: `spread-${s}-prev`,
            items,
            follows: { previousArtist, recent: null, window: null },
            order: spread.spreadByArtist(items, artistOf, spread.randomFromSeed(`spread-${s}-prev`), { previousArtist }),
        });
        const recent = items.filter((_, i) => i % 3 === 0);
        const isRecent = new Set(recent);
        for (const window of [3, 10]) {
            spreadCases.push({
                seed: `spread-${s}-recent-${window}`,
                items,
                follows: { previousArtist: "ar1", recent, window },
                order: spread.spreadByArtist(items, artistOf, spread.randomFromSeed(`spread-${s}-recent-${window}`), {
                    previousArtist: "ar1",
                    recent: (item) => isRecent.has(item),
                    window,
                }),
            });
        }
    }
});
goldens.spreadByArtist = spreadCases;

// replayPasses (services/mixes/stationOrder.ts): a seed's passes over a pool, each after the one before it (its last
// artist and its last songs kept out of the next opening), and a pass past REPLAY_PASSES built alone.
const stationPools = {
    one: [[uuid(0), "a0"]],
    five: Array.from({ length: 5 }, (_, i) => [uuid(i), `a${i}`]),
    thirty: Array.from({ length: 30 }, (_, i) => [uuid(i), `a${Math.floor(i / 3)}`]),
    big: Array.from({ length: 45 }, (_, i) => [uuid(i), i < 30 ? "big" : `o${i - 30}`]),
    mixed: Array.from({ length: 60 }, (_, i) => [uuid(i), i % 5 === 0 ? "a0" : `a${i % 7}`]),
    many: Array.from({ length: 250 }, (_, i) => [uuid(i), `a${Math.floor(i / 3)}`]),
};
const stationCases = [];
for (const [name, songs] of Object.entries(stationPools)) {
    const pool = songs.map(([id, artistId]) => ({ id, artistId }));
    const targets = name === "thirty" ? [0, 1, 2, 5, station.REPLAY_PASSES + 1] : [0, 1, 2, 5];
    for (const seed of ["x", "listener-1"]) {
        for (const target of targets) {
            const pass = station.replayPasses(seed, pool, target);
            stationCases.push({
                pool: name,
                seed,
                target,
                start: pass.start,
                pass: pass.pass,
                length: pass.length,
                lastArtist: pass.lastArtist,
                ids: pass.ids,
            });
        }
    }
}
goldens.stationOrder = {
    sessionTtlS: station.SESSION_TTL_S,
    stationsKept: station.STATIONS_KEPT,
    replayPasses: station.REPLAY_PASSES,
    replaySongs: station.REPLAY_SONGS,
    seedPattern: station.SEED_PATTERN.source,
    pools: stationPools,
    cases: stationCases,
};

goldens.weekdayOf = [
    "2026-10-25",
    "2026-10-25-1792771200000",
    "2026-10-31",
    "2026-11-19",
    "2026-02-30",
    "2026-13-45",
    "2026-00-00",
    "0099-01-01",
    "0100-01-01",
    "1970-01-01",
    "2000-02-29",
    "9999-12-31",
    "d7919",
    "2026-1-01",
    "",
].map((day) => {
    try {
        return [day, mixDay.weekdayOf(day)];
    } catch (e) {
        if (!(e instanceof RangeError)) throw e;
        return [day, null];
    }
});

fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, JSON.stringify(goldens) + "\n");
fs.rmSync(tmp, { recursive: true });
console.log(`wrote ${out}`);
