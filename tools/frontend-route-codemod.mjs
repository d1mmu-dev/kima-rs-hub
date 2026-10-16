// One-off (D-011): rewrite template-literal page links from /album/${id} to /album/?id=${...}.
// Usage: node tools/frontend-route-codemod.mjs frontend
import { readFileSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const RULES = [
    ["/podcasts/genre/", "/podcasts/genre/?genreId="],
    ["/audiobooks/series/", "/audiobooks/series/?name="],
    ["/browse/playlists/", "/browse/playlists/view/?id="],
    ["/podcasts/", "/podcasts/show/?id="],
    ["/audiobooks/", "/audiobooks/book/?id="],
    ["/album/", "/album/?id="],
    ["/artist/", "/artist/?id="],
    ["/playlist/", "/playlist/?id="],
    ["/mix/", "/mix/?id="],
    ["/share/", "/share/?token="],
];
// API paths, not page links.
const SKIP = new Set(["lib/api.ts"]);

function* files(dir) {
    for (const name of readdirSync(dir)) {
        if (name === "node_modules" || name === ".next" || name === "out") continue;
        const path = join(dir, name);
        if (statSync(path).isDirectory()) yield* files(path);
        else if (/\.(ts|tsx)$/.test(name)) yield path;
    }
}

function rewrite(text) {
    const re = /`(\/[A-Za-z/]+\/)\$\{/g;
    let out = "";
    let last = 0;
    let m;
    while ((m = re.exec(text))) {
        const rule = RULES.find(([from]) => from === m[1]);
        if (!rule) continue;
        const exprStart = m.index + m[0].length;
        let depth = 1;
        let j = exprStart;
        while (depth > 0) {
            if (j >= text.length) throw new Error("unbalanced ${ at " + m.index);
            const c = text[j++];
            if (c === "{") depth++;
            else if (c === "}") depth--;
        }
        const expr = text.slice(exprStart, j - 1);
        const value = expr.startsWith("encodeURIComponent(") ? expr : `encodeURIComponent(${expr})`;
        out += text.slice(last, m.index) + "`" + rule[1] + "${" + value + "}";
        last = j;
        re.lastIndex = j;
    }
    return out + text.slice(last);
}

const root = process.argv[2];
for (const path of files(root)) {
    const rel = relative(root, path);
    if (SKIP.has(rel)) continue;
    const before = readFileSync(path, "utf8");
    const after = rewrite(before);
    if (after !== before) {
        writeFileSync(path, after);
        console.log("rewrote", rel);
    }
}
