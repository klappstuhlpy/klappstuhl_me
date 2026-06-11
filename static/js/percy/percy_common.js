/* Percy Dashboard — shared helpers loaded on every dashboard page.
   Provides the global toast notifier and the nav "Features"/"Browse"
   dropdown behaviour that every page's header relies on. */

(function() {
    // -- Toast notifications ---------------------------------------------------
    // Single shared implementation; page scripts call window.showToast(...).
    if (typeof window.showToast === 'undefined') {
        window.showToast = function(level, message) {
            const toast = document.createElement('div');
            toast.className = 'toast toast-' + level;
            toast.textContent = message;
            document.body.appendChild(toast);
            requestAnimationFrame(() => toast.classList.add('visible'));
            setTimeout(() => {
                toast.classList.remove('visible');
                setTimeout(() => toast.remove(), 300);
            }, 3000);
        };
    }

    // -- Dashboard nav dropdowns (Features / Browse) ---------------------------
    document.querySelectorAll('.dashboard-nav .nav-group-label').forEach(function(label) {
        label.addEventListener('click', function() {
            const group = this.closest('.nav-group');
            const wasOpen = group.classList.contains('open');
            document.querySelectorAll('.dashboard-nav .nav-group').forEach(function(g) { g.classList.remove('open'); });
            if (!wasOpen) group.classList.add('open');
        });
    });
    document.addEventListener('click', function(e) {
        if (!e.target.closest('.nav-group')) {
            document.querySelectorAll('.dashboard-nav .nav-group').forEach(function(g) { g.classList.remove('open'); });
        }
    });
})();
