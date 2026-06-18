/* Homepage choreography:
     1. a text-scramble "decrypt" reveal for the name + tagline,
     2. a boot morph — a beat after load the terminal glides up and types in
        more lines, opening the page to its scroll-revealed content,
     3. scroll-reveal of the about / projects / stack sections,
     4. ambient drifting code + a cursor-following spotlight + a subtle
        terminal tilt.
   No dependencies. Honours prefers-reduced-motion (the markup already holds the
   final text and the CSS fallback is the expanded layout, so the quiet path is
   "do almost nothing"). */
(function () {
    "use strict";

    const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const finePointer = window.matchMedia("(pointer: fine)").matches;

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

    /* ── Boot morph ───────────────────────────────────────────────────
       A beat after the scramble settles, lift the terminal and reveal the
       extra lines + the page below. CSS does the actual animation; we just
       flip the `booted` class. Reduced-motion / no fine timing still ends in
       the same expanded state, just instantly. */
    function boot() { document.body.classList.add("booted"); }
    if (reduce) {
        boot();
    } else {
        // Long enough for "builds things for the web" (delay 850ms) to finish.
        window.setTimeout(boot, 2600);
    }

    /* ── Scroll-reveal ────────────────────────────────────────────────
       Reveal sections (and their staggered children) as they enter view. */
    const reveals = document.querySelectorAll(".reveal");
    if (reduce || !("IntersectionObserver" in window)) {
        reveals.forEach((el) => el.classList.add("in"));
    } else {
        const io = new IntersectionObserver((entries) => {
            for (const entry of entries) {
                if (entry.isIntersecting) {
                    entry.target.classList.add("in");
                    io.unobserve(entry.target);
                }
            }
        }, { threshold: 0.15, rootMargin: "0px 0px -8% 0px" });
        reveals.forEach((el) => io.observe(el));
    }

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

    /* ── Ambient drifting-code field ──────────────────────────────────
       A handful of code-ish tokens float up the page at low opacity. */
    const ambient = document.querySelector(".ambient");
    if (ambient && !reduce) {
        const TOKENS = [
            "fn main()", "{ }", "=>", "</>", "async", "impl", "0x1F", "use std::*",
            "match", "#[derive]", "let mut", "::", "pub fn", ".await", "Ok(())",
            "&str", "<T>", "||", "...", "return", "200 OK", "SELECT *", "git push",
            "None", "?", "==", "->", "loop {}", "0b1010", "#d97757",
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
    }

    /* ── Pointer effects (fine pointers only) ─────────────────────────
       • parallax nudge on the ambient field,
       • a soft spotlight that trails the cursor,
       • a subtle 3D tilt on the hero terminal. */
    if (finePointer && !reduce) {
        const glow = document.querySelector(".cursor-glow");
        const tilt = document.querySelector("[data-tilt]");
        let raf = 0;
        let mx = 0, my = 0;

        function apply() {
            raf = 0;
            if (ambient) {
                const x = (mx / window.innerWidth - 0.5) * 14;
                const y = (my / window.innerHeight - 0.5) * 14;
                ambient.style.transform = "translate(" + x.toFixed(1) + "px," + y.toFixed(1) + "px)";
            }
            if (glow) {
                glow.style.left = mx + "px";
                glow.style.top = my + "px";
            }
            if (tilt) {
                const r = tilt.getBoundingClientRect();
                const dx = (mx - (r.left + r.width / 2)) / r.width;   // -0.5..0.5
                const dy = (my - (r.top + r.height / 2)) / r.height;
                // Only tilt when the cursor is reasonably near the window.
                if (Math.abs(dx) < 1.4 && Math.abs(dy) < 1.4) {
                    tilt.style.setProperty("--ry", (dx * 6).toFixed(2) + "deg");
                    tilt.style.setProperty("--rx", (-dy * 6).toFixed(2) + "deg");
                } else {
                    tilt.style.setProperty("--ry", "0deg");
                    tilt.style.setProperty("--rx", "0deg");
                }
            }
        }

        window.addEventListener("mousemove", (e) => {
            mx = e.clientX; my = e.clientY;
            if (glow) glow.classList.add("on");
            if (!raf) raf = requestAnimationFrame(apply);
        }, { passive: true });

        window.addEventListener("mouseleave", () => {
            if (glow) glow.classList.remove("on");
            if (tilt) { tilt.style.setProperty("--ry", "0deg"); tilt.style.setProperty("--rx", "0deg"); }
        }, { passive: true });

        // Per-card sheen: feed the local cursor position into the card's
        // radial-gradient hotspot (--mx/--my used by .project-card::after).
        document.querySelectorAll(".project-card").forEach((card) => {
            card.addEventListener("mousemove", (e) => {
                const r = card.getBoundingClientRect();
                card.style.setProperty("--mx", (e.clientX - r.left) + "px");
                card.style.setProperty("--my", (e.clientY - r.top) + "px");
            }, { passive: true });
        });
    }
})();
