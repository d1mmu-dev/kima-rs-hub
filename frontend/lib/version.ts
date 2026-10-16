import packageJson from "../package.json";

const BASE_VERSION = packageJson.version;

// NEXT_PUBLIC_BUILD_TYPE marks non-release builds in the UI: "prerelease" (the
// ghcr.io channel) or "dev" (local stack). A tagged release passes nothing and
// shows a bare version; any other value is shown verbatim.
const BUILD_TYPE = process.env.NEXT_PUBLIC_BUILD_TYPE?.trim() || "";

export const APP_VERSION = BUILD_TYPE ? `${BASE_VERSION}-${BUILD_TYPE}` : BASE_VERSION;
