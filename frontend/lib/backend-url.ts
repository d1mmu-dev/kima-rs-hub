/* Where server-side route handlers reach the backend. Images set BACKEND_URL;
 * the fallback is the local dev backend (backend/.env PORT=3111). */
export const backendUrl =
    process.env.BACKEND_URL || process.env.NEXT_PUBLIC_BACKEND_URL || "http://127.0.0.1:3111";
