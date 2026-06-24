/* Homepage:
   1. scroll-reveal of the projects / stack sections,
   2. ambient drifting code + cursor-following spotlight.
   No dependencies. Honours prefers-reduced-motion. */
(function () {
    "use strict";

    document.documentElement.classList.add("js-index");

    /* ── Scroll-reveal ────────────────────────────────────────────────
    /* Hide the scroll cue once the visitor has scrolled at all. */
    let scrolled = false;
    window.addEventListener("scroll", () => {
        if (!scrolled && window.scrollY > 24) {
            scrolled = true;
            document.body.classList.add("scrolled");
        } else if (scrolled && window.scrollY <= 24) {
            scrolled = false;
            document.body.classList.remove("scrolled");
        }
    }, { passive: true });
})();