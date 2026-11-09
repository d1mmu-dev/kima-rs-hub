import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SectionHeader } from "@/components/ui/SectionHeader";
import { PageMessage } from "@/components/ui/PageMessage";

describe("v1.12.3 shared heading hierarchy", () => {
    it("renders the compact section heading without a large section gap", () => {
        const markup = renderToStaticMarkup(<SectionHeader title="About" size="xs" />);
        expect(markup).toContain("mb-1 gap-2");
        expect(markup).toContain("h-3.5");
        expect(markup).toContain("text-sm");
    });

    it("uses the shared page title role on empty-state pages", () => {
        const markup = renderToStaticMarkup(<PageMessage title="No albums" description="Try again" />);
        expect(markup).toContain('<h1 class="t-page-title">No albums</h1>');
    });
});
