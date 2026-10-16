"use client";

import { useEffect } from "react";
import { PageMessage } from "@/components/ui/PageMessage";

export default function Error({ error, reset }: { error: Error & { digest?: string }; reset: () => void }) {
    useEffect(() => {
        console.error("Route error:", error);
    }, [error]);

    return (
        <PageMessage
            title="something went wrong"
            action={{ label: "try again", onClick: reset }}
        />
    );
}
