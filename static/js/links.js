// Behaviour for the URL-shortener management page (/links):
// copy-to-clipboard, the edit modal, and delete confirmation.
(() => {
    "use strict";

    // --- Copy short link to clipboard ---------------------------------------
    document.querySelectorAll(".link-copy").forEach((btn) => {
        btn.addEventListener("click", async () => {
            const url = btn.dataset.url;
            if (!url) return;
            try {
                await navigator.clipboard.writeText(url);
                const original = btn.textContent;
                btn.textContent = "✓";
                btn.classList.add("copied");
                setTimeout(() => {
                    btn.textContent = original;
                    btn.classList.remove("copied");
                }, 1200);
            } catch {
                if (window.showToast) showToast("error", "Could not copy to clipboard.");
            }
        });
    });

    // --- Edit modal ---------------------------------------------------------
    const modal = document.getElementById("edit-link-modal");
    const form = document.getElementById("edit-link-form");
    const targetInput = document.getElementById("edit-target");
    const aliasInput = document.getElementById("edit-alias");

    if (modal && form) {
        document.querySelectorAll(".link-edit").forEach((btn) => {
            btn.addEventListener("click", () => {
                const row = btn.closest(".links-row");
                if (!row) return;
                form.action = `/links/${row.dataset.id}/edit`;
                targetInput.value = row.dataset.target || "";
                aliasInput.value = row.dataset.code || "";
                modal.showModal();
            });
        });

        const cancel = document.getElementById("edit-cancel");
        if (cancel) cancel.addEventListener("click", () => modal.close());
    }

    // --- Delete confirmation ------------------------------------------------
    document.querySelectorAll(".link-delete-form").forEach((deleteForm) => {
        deleteForm.addEventListener("submit", (e) => {
            const row = deleteForm.closest(".links-row");
            const code = row ? row.dataset.code : "this link";
            if (!confirm(`Delete short link "${code}"? This can't be undone.`)) {
                e.preventDefault();
            }
        });
    });
})();
