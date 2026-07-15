// "Your pastes" (/pastes): client-side filtering of an already-rendered grid.
//
// The whole list is on the page, so filtering is a class toggle rather than a
// round trip. With JS off you simply get the unfiltered grid, which is fine.

(function () {
    "use strict";

    const grid = document.getElementById("paste-grid");
    if (!grid) return;

    const cards = [...grid.querySelectorAll(".paste-card")];
    const search = document.getElementById("paste-search");
    const languageFilter = document.getElementById("filter-language");
    const empty = document.getElementById("no-results");

    function selectedVisibility() {
        const checked = document.querySelector('input[name="filter-visibility"]:checked');
        return checked ? checked.value : "";
    }

    function apply() {
        const query = (search?.value || "").trim().toLowerCase();
        const language = languageFilter?.value || "";
        const visibility = selectedVisibility();
        let shown = 0;

        cards.forEach((card) => {
            const matchesQuery =
                !query || card.dataset.title.includes(query) || card.dataset.id.toLowerCase().includes(query);
            const matchesLanguage = !language || card.dataset.language === language;
            const matchesVisibility = !visibility || card.dataset.visibility === visibility;

            const visible = matchesQuery && matchesLanguage && matchesVisibility;
            card.hidden = !visible;
            if (visible) shown++;
        });

        if (empty) empty.hidden = shown !== 0;
    }

    search?.addEventListener("input", apply);
    languageFilter?.addEventListener("change", apply);
    document
        .querySelectorAll('input[name="filter-visibility"]')
        .forEach((radio) => radio.addEventListener("change", apply));

    document.querySelectorAll("form[data-confirm]").forEach((form) => {
        form.addEventListener("submit", (event) => {
            if (!window.confirm(form.dataset.confirm)) event.preventDefault();
        });
    });
})();
