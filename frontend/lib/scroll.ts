/** Scrolls the app's main column, the element that scrolls on every page, back to its top. */
export function scrollMainToTop() {
    document.getElementById("main-content")?.scrollTo({ top: 0, behavior: "instant" });
}
