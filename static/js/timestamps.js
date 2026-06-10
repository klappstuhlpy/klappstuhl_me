// Humanises ISO-8601 timestamps marked with `.js-ts`.
//
// Each `.js-ts` element carries the raw value in its `datetime` attribute (or
// its text content). The visible text becomes a relative phrase ("2 hours ago",
// "in 3 days") and the `title` attribute holds the full local date for hover.
// Invalid / placeholder values (e.g. "—") are left untouched. Idempotent — safe
// to call again after injecting new DOM via `window.formatTimestamps(root)`.
(function () {
    "use strict";

    var ABS_OPTS = {
        weekday: "long", year: "numeric", month: "long", day: "numeric",
        hour: "numeric", minute: "2-digit",
    };

    function formatAbsolute(d) {
        try {
            return d.toLocaleString(undefined, ABS_OPTS);
        } catch (e) {
            return d.toString();
        }
    }

    function relativeFormatter() {
        if (typeof Intl !== "undefined" && Intl.RelativeTimeFormat) {
            return new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
        }
        return null;
    }

    var RTF = relativeFormatter();

    function rel(value, unit) {
        if (RTF) return RTF.format(value, unit);
        var n = Math.abs(value);
        var s = n + " " + unit + (n === 1 ? "" : "s");
        return value < 0 ? s + " ago" : "in " + s;
    }

    function formatRelative(d) {
        var diffMs = d.getTime() - Date.now(); // > 0 => future
        var sign = diffMs < 0 ? -1 : 1;
        var sec = Math.round(Math.abs(diffMs) / 1000);
        var min = Math.round(sec / 60);
        var hr = Math.round(min / 60);
        var day = Math.round(hr / 24);

        if (sec < 45) return rel(sign * sec, "second");
        if (min < 45) return rel(sign * min, "minute");
        if (hr < 22) return rel(sign * hr, "hour");
        if (day < 26) return rel(sign * day, "day");
        var month = Math.round(day / 30);
        if (month < 11) return rel(sign * month, "month");
        return rel(sign * Math.round(day / 365), "year");
    }

    function formatOne(el) {
        var raw = (el.getAttribute("datetime") || el.textContent || "").trim();
        if (!raw) return;
        var d = new Date(raw);
        if (isNaN(d.getTime())) return; // not a real date (placeholder dash, etc.)

        el.setAttribute("datetime", raw);
        el.setAttribute("title", formatAbsolute(d));
        el.textContent = formatRelative(d);
        el.classList.add("ts-formatted");
    }

    function run(root) {
        var scope = root && root.querySelectorAll ? root : document;
        scope.querySelectorAll(".js-ts:not(.ts-formatted)").forEach(formatOne);
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () { run(); });
    } else {
        run();
    }

    window.formatTimestamps = run;
})();
