// Regression tests for tools/frontend-route-codemod.mjs (D-011). Run: node --test tools/frontend-route-codemod.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { rewrite } from "./frontend-route-codemod.mjs";

test("podcast category links become the static category query route", () => {
    assert.equal(
        rewrite("router.push(`/podcasts/category/${category.id}`)"),
        "router.push(`/podcasts/category/?id=${encodeURIComponent(category.id)}`)",
    );
});

test("podcast show links still become the show query route", () => {
    assert.equal(rewrite("href={`/podcasts/${p.id}`}"), "href={`/podcasts/show/?id=${encodeURIComponent(p.id)}`}");
});

test("settings sub-pages stay path links: the static export builds one page per slug", () => {
    const src = "href={`/settings/${page.slug}`}";
    assert.equal(rewrite(src), src);
});

test("detail links keep an already-encoded value and nested braces", () => {
    assert.equal(
        rewrite("`/mix/${encodeURIComponent(mix.id)}` `/album/${a[{x:1}.x]}`"),
        "`/mix/?id=${encodeURIComponent(mix.id)}` `/album/?id=${encodeURIComponent(a[{x:1}.x])}`",
    );
});

test("rewriting is idempotent", () => {
    const once = rewrite("`/podcasts/category/${id}` `/artist/${id}`");
    assert.equal(rewrite(once), once);
});

test("usePathname comes from the trailing-slash-free wrapper", () => {
    assert.equal(
        rewrite('import { usePathname, useRouter, useSearchParams } from "next/navigation";\n'),
        'import { useRouter, useSearchParams } from "next/navigation";\nimport { usePathname } from "@/lib/use-pathname";\n',
    );
    assert.equal(
        rewrite('import { usePathname } from "next/navigation";\n'),
        'import { usePathname } from "@/lib/use-pathname";\n',
    );
});
