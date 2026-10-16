import { Button } from "@/components/ui/Button";
import { InlineStatus } from "@/components/ui/InlineStatus";
import type { useConnectionTest } from "../../hooks/useConnectionTest";

interface ConnectionTestButtonProps {
    connection: ReturnType<typeof useConnectionTest>;
    disabled: boolean;
}

export function ConnectionTestButton({ connection, disabled }: ConnectionTestButtonProps) {
    return (
        <div className="inline-flex items-center gap-3">
            <Button variant="secondary" size="sm" onClick={connection.test} disabled={disabled}>
                {connection.status === "loading" ? connection.busy : "Test Connection"}
            </Button>
            <InlineStatus
                status={connection.status}
                message={connection.message}
                onClear={connection.clear}
            />
        </div>
    );
}
