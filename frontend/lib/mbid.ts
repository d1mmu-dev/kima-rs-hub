const MBID_FORMAT = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** True for a value shaped like a MusicBrainz id; temp- placeholders and row ids fail. */
export function isMbid(value: string | null | undefined): value is string {
    return typeof value === "string" && MBID_FORMAT.test(value);
}
