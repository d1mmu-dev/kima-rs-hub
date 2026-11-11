// D-011: rewrite template-literal page links from /album/${id} to /album/?id=${...}, and take
// usePathname from the trailing-slash-free wrapper. Re-run after importing upstream frontend files.
// Usage: node tools/frontend-route-codemod.mjs frontend
import { readFileSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { pathToFileURL } from "node:url";

// `/settings/${slug}` is deliberately absent: the export builds one page per settings slug (D-011),
// so those links stay path links. Upstream removed `/podcasts/genre/` in v1.12.3.
const RULES = [
    ["/podcasts/category/", "/podcasts/category/?id="],
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
// API paths, not page links; the wrapper itself must keep Next's hook.
const SKIP = new Set(["lib/api.ts", "lib/use-pathname.ts"]);

function* files(dir) {
    for (const name of readdirSync(dir)) {
        if (name === "node_modules" || name === ".next" || name === "out") continue;
        const path = join(dir, name);
        if (statSync(path).isDirectory()) yield* files(path);
        else if (/\.(ts|tsx)$/.test(name)) yield path;
    }
}

export function rewrite(text) {
    return rewriteLinks(rewritePathname(text));
}

// `trailingSlash: true` adds a slash Next's own usePathname reports; the wrapper strips it (D-011).
function rewritePathname(text) {
    return text.replace(
        /^import \{([^}]*)\} from "next\/navigation";\n/m,
        (line, names) => {
            const list = names.split(",").map((n) => n.trim()).filter(Boolean);
            if (!list.includes("usePathname")) return line;
            const rest = list.filter((n) => n !== "usePathname");
            const wrapper = 'import { usePathname } from "@/lib/use-pathname";\n';
            return rest.length ? `import { ${rest.join(", ")} } from "next/navigation";\n${wrapper}` : wrapper;
        },
    );
}

function rewriteLinks(text) {
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

function main(root) {
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
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main(process.argv[2]);
