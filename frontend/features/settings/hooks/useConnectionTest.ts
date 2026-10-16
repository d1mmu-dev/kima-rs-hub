import { useState } from "react";
import type { StatusType } from "@/components/ui/InlineStatus";
import type { ConnectionTestResult } from "../types";

interface TestLabels {
    busy: string;
    connected: (version?: string) => string;
    failed: string;
}

const defaultLabels: TestLabels = {
    busy: "Testing...",
    connected: (version) => (version ? `v${version}` : "Connected"),
    failed: "Failed",
};

/** The state behind a "Test Connection" button and the status shown beside it. */
export function useConnectionTest(
    run: () => Promise<ConnectionTestResult>,
    { onConnected, ...labels }: Partial<TestLabels> & { onConnected?: () => void } = {},
) {
    const [status, setStatus] = useState<StatusType>("idle");
    const [message, setMessage] = useState("");
    const { busy, connected, failed } = { ...defaultLabels, ...labels };

    const test = async () => {
        setStatus("loading");
        setMessage(busy);
        const result = await run();
        if (result.success) {
            setStatus("success");
            setMessage(connected(result.version));
            onConnected?.();
        } else {
            setStatus("error");
            setMessage(result.error || failed);
        }
    };

    return { status, message, busy, test, clear: () => setStatus("idle") };
}
