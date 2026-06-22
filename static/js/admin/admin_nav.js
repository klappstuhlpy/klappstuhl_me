/* This file is licensed under AGPL-3.0 */

/* Admin sidebar collapse toggle (mobile).
   On desktop the nav is always visible; on narrow viewports it collapses behind
   a "Menu" button (.admin-sidebar-toggle) that flips the .open class on the
   sidebar — mirroring the Percy dashboard's mobile nav. */
(function () {
    const sidebar = document.getElementById('admin-sidebar');
    const toggle = sidebar && sidebar.querySelector('.admin-sidebar-toggle');
    if (!toggle) return;
    toggle.addEventListener('click', function () {
        const open = sidebar.classList.toggle('open');
        toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
    });
})();
