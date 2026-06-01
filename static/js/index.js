/* Homepage animation: a text-scramble "decrypt" reveal for the name and
   tagline, layered over a field of faint monospace code tokens drifting
   upward. No dependencies. Honours prefers-reduced-motion (the markup
   already contains the final text, so doing nothing is a valid state). */
(function () {
    "use strict";

    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    /* ── Scramble decrypt-reveal ──────────────────────────────────────
       Each character flickers through random glyphs, then locks into its
       final letter on a staggered, left-to-right schedule. */
    const GLYPHS = "!<>-_\\/[]{}=+*^?#01;:&%$".split("");

    function scramble(el, delayMs) {
        const final = el.dataset.final || el.textContent;
        if (reduce) { el.textContent = final; return; }

        const chars = final.split("").map((ch, i) => ({
            ch,
            // Frame at which this character stops flickering and locks in.
            reveal: 12 + i * 3 + Math.floor(Math.random() * 12),
        }));

        let frame = 0;
        const startAt = performance.now() + (delayMs || 0);

        function tick(now) {
            if (now < startAt) { requestAnimationFrame(tick); return; }

            let html = "";
            let done = 0;
            for (const c of chars) {
                if (c.ch === " ") { html += " "; done++; continue; }
                if (frame >= c.reveal) {
                    html += c.ch;
                    done++;
                } else {
                    const g = GLYPHS[(Math.random() * GLYPHS.length) | 0];
                    html += '<span class="glitch">' + g + "</span>";
                }
            }
            el.innerHTML = html;
            frame++;

            if (done < chars.length) {
                requestAnimationFrame(tick);
            } else {
                el.textContent = final; // settle to clean text (drops <span>s)
            }
        }
        requestAnimationFrame(tick);
    }

    document.querySelectorAll(".scramble").forEach((el) => {
        scramble(el, parseInt(el.dataset.delay || "0", 10));
    });

    /* ── Ambient drifting-code field ──────────────────────────────────
       A handful of code-ish tokens float up the page at low opacity. */
    const ambient = document.querySelector(".ambient");
    if (ambient && !reduce) {
        const TOKENS = [
            "fn main()", "{ }", "=>", "</>", "async", "impl", "0x1F", "use std::*",
            "match", "#[derive]", "let mut", "::", "pub fn", ".await", "Ok(())",
            "&str", "<T>", "||", "...", "return", "200 OK", "SELECT *", "git push",
            "None", "?", "==", "->", "loop {}", "0b1010", "#7c3aed",
        ];
        const count = window.innerWidth < 600 ? 14 : 28;
        const frag = document.createDocumentFragment();

        for (let i = 0; i < count; i++) {
            const span = document.createElement("span");
            span.className = "code-bit";
            span.textContent = TOKENS[(Math.random() * TOKENS.length) | 0];

            const dur = 16 + Math.random() * 26; // 16–42s per pass
            span.style.left = (Math.random() * 100).toFixed(2) + "%";
            span.style.fontSize = (0.7 + Math.random() * 0.9).toFixed(2) + "rem";
            span.style.animationDuration = dur.toFixed(1) + "s";
            // Negative delay so the field is already populated on first paint.
            span.style.animationDelay = (-Math.random() * dur).toFixed(1) + "s";
            span.style.setProperty("--drift", (Math.random() * 40 - 20).toFixed(1) + "px");

            frag.appendChild(span);
        }
        ambient.appendChild(frag);

        // Subtle parallax: nudge the whole field toward the cursor. Pointer
        // devices only — no-op on touch where there's no hover position.
        if (window.matchMedia("(pointer: fine)").matches) {
            window.addEventListener("mousemove", (e) => {
                const x = (e.clientX / window.innerWidth - 0.5) * 14;
                const y = (e.clientY / window.innerHeight - 0.5) * 14;
                ambient.style.transform = "translate(" + x.toFixed(1) + "px," + y.toFixed(1) + "px)";
            }, { passive: true });
        }
    }
})();
