/**
 * Numbers as a person would say them. Time derived from track length is
 * introduced with "about"; time from a real playback position never is.
 * Nothing rounds to a lie: 0.4 hours is "24 minutes", not "0 hours".
 */

/** Just the figure, for when a label already says what it is. */
export function hoursValue(seconds: number): string {
    const h = seconds / 3600;
    if (h < 10 && h > 0) return h.toFixed(1);
    return Math.round(h).toLocaleString();
}

export function bytes(n: number): string {
    if (n <= 0) return "0";
    const units = ["B", "KB", "MB", "GB", "TB"];
    const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
    const v = n / 1024 ** i;
    return `${v < 10 && i > 0 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function uptime(seconds: number): string {
    const d = Math.floor(seconds / 86400);
    const h = Math.floor((seconds % 86400) / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    if (d > 0) return `${d}d ${h}h`;
    if (h > 0) return `${h}h ${m}m`;
    return `${m}m`;
}

const WEEKDAYS = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

/** "1st", "2nd", "45th"; 11-13 take "th", so the last two digits decide first. */
export function ordinal(n: number): string {
    const lastTwo = n % 100;
    if (lastTwo >= 11 && lastTwo <= 13) return `${n}th`;
    switch (n % 10) {
        case 1: return `${n}st`;
        case 2: return `${n}nd`;
        case 3: return `${n}rd`;
        default: return `${n}th`;
    }
}

/** "9pm" — one hour of the day, said the way a person says it. */
export function hourLabel(hour: number): string {
    return `${hour % 12 === 0 ? 12 : hour % 12}${hour < 12 ? "am" : "pm"}`;
}

/** "thursday 9pm" — a weekday and an hour. */
export function whenLabel(weekday: number, hour: number): string {
    return `${WEEKDAYS[weekday]} ${hourLabel(hour)}`;
}

export function monthLabel(iso: string): string {
    const [y, m] = iso.split("-");
    const names = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    return `${names[Number(m) - 1]} ${y.slice(2)}`;
}

export function monthYear(iso: string | null): string {
    if (!iso) return "";
    const d = new Date(iso);
    return d.toLocaleDateString(undefined, { month: "long", year: "numeric" }).toLowerCase();
}

export function percent(part: number, whole: number): number {
    if (whole <= 0) return 0;
    return Math.round((part / whole) * 100);
}

/** "september 2025" — where "sep 25" would read as a day of the month. */
export function monthLong(iso: string): string {
    const [y, m] = iso.split("-");
    const names = ["january", "february", "march", "april", "may", "june",
        "july", "august", "september", "october", "november", "december"];
    return `${names[Number(m) - 1]} ${y}`;
}
