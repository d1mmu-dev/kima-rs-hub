"use client";

import { usePathname as useNextPathname } from "next/navigation";

/**
 * `usePathname` without the trailing slash that `trailingSlash: true` (static
 * export, D-011) adds, so comparisons like `pathname === "/login"` keep working.
 * The root stays "/".
 */
export function usePathname(): string {
    const pathname = useNextPathname() ?? "/";
    return pathname.length > 1 && pathname.endsWith("/") ? pathname.slice(0, -1) : pathname;
}
