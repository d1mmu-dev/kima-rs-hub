import { NextRequest } from "next/server";
import { backendUrl } from "@/lib/backend-url";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** Streams the backend's event stream through, because Next.js rewrites buffer it. */
export async function GET(request: NextRequest) {
    const ticket = request.nextUrl.searchParams.get("ticket");
    if (!ticket) {
        return new Response("Unauthorized", { status: 401 });
    }

    const abortController = new AbortController();

    // Abort the backend request when the client disconnects
    request.signal.addEventListener("abort", () => {
        abortController.abort();
    });

    let backendResponse: Response;
    try {
        backendResponse = await fetch(
            `${backendUrl}/api/events?ticket=${encodeURIComponent(ticket)}`,
            {
                headers: {
                    Accept: "text/event-stream",
                    "Cache-Control": "no-cache",
                },
                signal: abortController.signal,
            }
        );
    } catch {
        return new Response("Backend unavailable", { status: 502 });
    }

    if (!backendResponse.ok) {
        const body = await backendResponse.text();
        return new Response(body, { status: backendResponse.status });
    }

    if (!backendResponse.body) {
        return new Response("No stream from backend", { status: 502 });
    }

    return new Response(backendResponse.body, {
        headers: {
            "Content-Type": "text/event-stream",
            "Cache-Control": "no-cache",
            Connection: "keep-alive",
            "X-Accel-Buffering": "no",
        },
    });
}
