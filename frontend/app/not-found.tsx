import { PageMessage } from "@/components/ui/PageMessage";

export default function NotFound() {
    return (
        <PageMessage
            title="Page not found"
            description="Page not found."
            action={{ label: "Home", href: "/" }}
        />
    );
}
