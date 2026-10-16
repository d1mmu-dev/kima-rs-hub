import { Suspense } from "react";
import SharePageClient from "./SharePageClient";

export default function SharePage() {
    return (
        <Suspense fallback={null}>
            <SharePageClient />
        </Suspense>
    );
}
