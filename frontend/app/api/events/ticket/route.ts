import { NextRequest } from "next/server";
import { backendUrl } from "@/lib/backend-url";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

/** Forwards ticket requests, since the rewrites skip /api/events/* to leave room for the stream route. */
export async function POST(request: NextRequest) {
    const authHeader = request.headers.get("Authorization");
    if (!authHeader) {
        return new Response("Unauthorized", { status: 401 });
    }

    let backendResponse: Response;
    try {
        backendResponse = await fetch(`${backendUrl}/api/events/ticket`, {
            method: "POST",
            headers: {
                Authorization: authHeader,
                "Content-Type": "application/json",
            },
        });
    } catch {
        return new Response("Backend unavailable", { status: 502 });
    }

    const body = await backendResponse.text();
    return new Response(body, {
        status: backendResponse.status,
        headers: { "Content-Type": "application/json" },
    });
}
