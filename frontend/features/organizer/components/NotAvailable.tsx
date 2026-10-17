import { PageMessage } from "@/components/ui/PageMessage";

/** Shown instead of a page whose subsystem is not ported yet, or that needs an admin. */
export function NotAvailable({ title = "Not available yet", description }: { title?: string; description?: string }) {
    return (
        <PageMessage
            title={title}
            description={description ?? "This part of kima is not available on this server yet."}
            action={{ label: "Back to home", href: "/" }}
        />
    );
}
