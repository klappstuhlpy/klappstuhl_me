/* Copy-invite-URL buttons on /admin/invites */

document.querySelectorAll(".copy-invite").forEach((btn) => {
  btn.addEventListener("click", async () => {
    const card = btn.closest(".invite-card");
    const urlEl = card?.querySelector(".invite-url");
    const url = urlEl?.dataset.url ?? urlEl?.textContent?.trim();
    if (!url) return;

    try {
      await navigator.clipboard.writeText(url);
      const original = btn.textContent;
      btn.textContent = "Copied!";
      btn.disabled = true;
      setTimeout(() => {
        btn.textContent = original;
        btn.disabled = false;
      }, 1200);
    } catch (e) {
      console.error("clipboard write failed:", e);
      btn.textContent = "Copy failed";
      setTimeout(() => (btn.textContent = "Copy URL"), 1500);
    }
  });
});

/* Confirm revoke */
document.querySelectorAll(".invite-card form .button.danger").forEach((btn) => {
  btn.addEventListener("click", (e) => {
    if (!confirm("Revoke this invite? It can no longer be used.")) {
      e.preventDefault();
    }
  });
});
