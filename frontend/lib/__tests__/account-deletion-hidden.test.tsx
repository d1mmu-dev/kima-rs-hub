/**
 * kima-rs (D-170, D-180): account deletion (DELETE /api/auth/account) is not served yet, so Settings -> You ->
 * Account leaves the row out instead of offering a button that fails. Upstream's own test covers the row.
 */
import { describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { mountHost } from "./helpers/dom";

vi.mock("@/lib/auth-context", () => ({
    useAuth: () => ({ user: { id: "u1", username: "someone", role: "user" }, deleteAccount: vi.fn() }),
}));
vi.mock("@/lib/api", () => ({ api: { get: vi.fn(async () => ({})), post: vi.fn(async () => ({})) } }));
vi.mock("@/features/settings/hooks/useTwoFactor", () => ({
    useTwoFactor: () => ({ twoFactorEnabled: false, recoveryCodes: [], showRecoveryCodes: false, load2FAStatus: vi.fn(), setup2FA: vi.fn() }),
}));

import { AccountSection } from "@/features/settings/components/sections/AccountSection";
import { SectionFlagsContext, type SectionFlags } from "@/lib/section-flags";

const dom = mountHost();
const flags = (accountDeletion: boolean): SectionFlags => ({
    podcasts: false, audiobooks: false, discover: false, mixes: false, downloads: false, imports: false,
    stats: false, changelog: false, accountDeletion,
});

describe("account deletion on kima-rs", () => {
    it("is not offered while the server has no account deletion", () => {
        act(() =>
            dom.root.render(
                <SectionFlagsContext.Provider value={flags(false)}>
                    <AccountSection />
                </SectionFlagsContext.Provider>,
            ),
        );
        expect(dom.host.textContent).not.toContain("Delete account");
    });

    it("is offered once the server has it", () => {
        act(() =>
            dom.root.render(
                <SectionFlagsContext.Provider value={flags(true)}>
                    <AccountSection />
                </SectionFlagsContext.Provider>,
            ),
        );
        expect(dom.host.textContent).toContain("Delete account");
    });
});
