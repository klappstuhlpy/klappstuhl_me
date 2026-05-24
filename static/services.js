/* ── Service card button state ───────────────────────────────── */
document.addEventListener("DOMContentLoaded", () => {
  document.querySelectorAll(".service-card").forEach(card => {
    const isRunning = card.dataset.running === "true";
    const startBtn   = card.querySelector(".start-btn");
    const restartBtn = card.querySelector(".restart-btn");
    const stopBtn    = card.querySelector(".stop-btn");

    if (isRunning) {
      startBtn.disabled   = true;
      restartBtn.disabled = false;
      stopBtn.disabled    = false;
    } else {
      startBtn.disabled   = false;
      restartBtn.disabled = true;
      stopBtn.disabled    = true;
    }
  });
});

/* ── Log console ─────────────────────────────────────────────── */

const modal         = document.getElementById("log-modal");
const terminal      = document.getElementById("log-terminal");
const modalName     = document.getElementById("log-modal-name");
const connDot       = document.getElementById("log-connection-dot");
const connLabel     = document.getElementById("log-connection-label");
const closeBtn      = document.getElementById("log-close-btn");
const clearBtn      = document.getElementById("log-clear-btn");
const autoscrollCbx = document.getElementById("autoscroll-toggle");

let activeSource = null;

/* ── Log line highlighting ───────────────────────────────────── */

/** Strip ANSI/VT escape sequences. */
function stripAnsi(str) {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "");
}

/** Escape the five HTML special characters. */
function escHtml(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Patterns are listed highest-priority first.
 * When two matches overlap the one that starts earlier wins; if they start
 * at the same position the longer (more specific) match wins.
 */
const LOG_PATTERNS = [
  // ISO-8601 / Docker timestamps
  { re: /\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?/g, cls: "hl-ts" },
  // Log levels — error
  { re: /\b(?:ERROR|FATAL|CRITICAL|PANIC|EXCEPTION|EMERG|ALERT)\b/gi,               cls: "hl-error" },
  // Log levels — warning
  { re: /\bWARN(?:ING)?\b/gi,                                                        cls: "hl-warn" },
  // Log levels — info
  { re: /\b(?:INFO|NOTICE|SUCCESS)\b/gi,                                             cls: "hl-info" },
  // Log levels — debug
  { re: /\b(?:DEBUG|TRACE|VERBOSE)\b/gi,                                             cls: "hl-debug" },
  // HTTP methods
  { re: /\b(?:GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|CONNECT)\b/g,                   cls: "hl-method" },
  // URLs
  { re: /https?:\/\/[^\s"<>]+/g,                                                     cls: "hl-url" },
  // HTTP status 4xx / 5xx
  { re: /\b[45]\d{2}\b/g,                                                            cls: "hl-status-err" },
  // HTTP status 1xx / 2xx / 3xx
  { re: /\b[123]\d{2}\b/g,                                                           cls: "hl-status-ok" },
  // Double-quoted strings
  { re: /"(?:[^"\\]|\\.)*"/g,                                                        cls: "hl-string" },
  // IP address (optionally with port)
  { re: /\b\d{1,3}(?:\.\d{1,3}){3}(?::\d{1,5})?\b/g,                               cls: "hl-ip" },
  // Durations: 123ms, 1.5s, 800µs, 400ns
  { re: /\b\d+(?:\.\d+)?(?:µs|ms|ns|us|s|m|h)\b/g,                                 cls: "hl-duration" },
  // Plain numbers (lowest priority)
  { re: /\b\d+(?:\.\d+)?\b/g,                                                        cls: "hl-num" },
];

/**
 * Returns an HTML string for one log line with semantic spans.
 * Strips ANSI codes, HTML-escapes all plain text, and wraps matched
 * tokens in <span class="hl-*"> elements.  Patterns never overlap —
 * higher-priority (earlier in the list) matches shadow lower ones.
 */
function highlightLogLine(raw) {
  const text = stripAnsi(raw);

  // Collect every match from every pattern
  const matches = [];
  for (const { re, cls } of LOG_PATTERNS) {
    re.lastIndex = 0;
    let m;
    while ((m = re.exec(text)) !== null) {
      matches.push({ start: m.index, end: m.index + m[0].length, cls, src: m[0] });
    }
  }

  // Sort: earlier start first; same start → longer match first
  matches.sort((a, b) => a.start - b.start || b.end - a.end);

  // Walk left-to-right, keeping only non-overlapping tokens
  const tokens = [];
  let cursor = 0;
  for (const tok of matches) {
    if (tok.start >= cursor) {
      tokens.push(tok);
      cursor = tok.end;
    }
  }

  // Rebuild the line as HTML
  let html = "";
  let pos = 0;
  for (const { start, end, cls, src } of tokens) {
    if (start > pos) html += escHtml(text.slice(pos, start));
    html += `<span class="${cls}">${escHtml(src)}</span>`;
    pos = end;
  }
  if (pos < text.length) html += escHtml(text.slice(pos));
  return html;
}

function setConnectionState(state) {
  connDot.className   = "log-connection-dot " + state;    // connecting | connected | closed | error
  const labels = { connecting: "Connecting…", connected: "Connected", closed: "Closed", error: "Error" };
  connLabel.textContent = labels[state] ?? state;
}

function appendLine(text) {
  const line = document.createElement("div");
  line.className = "log-line";
  line.innerHTML = highlightLogLine(text);   // stripAnsi + escHtml called inside
  terminal.appendChild(line);

  if (autoscrollCbx.checked) {
    terminal.scrollTop = terminal.scrollHeight;
  }
}

function openLogs(serviceName) {
  closeLogs();                          // close any existing connection first
  terminal.innerHTML = "";
  modalName.textContent = serviceName + " — logs";
  setConnectionState("connecting");
  modal.showModal();

  activeSource = new EventSource(`/services/logs/${encodeURIComponent(serviceName)}`);

  activeSource.onopen = () => setConnectionState("connected");

  activeSource.onmessage = (e) => appendLine(e.data);

  activeSource.onerror = () => {
    setConnectionState("error");
    activeSource.close();
    activeSource = null;
  };
}

function closeLogs() {
  if (activeSource) {
    activeSource.close();
    activeSource = null;
  }
  setConnectionState("closed");
}

/* Wire up the Logs button on each service card */
document.querySelectorAll(".logs-btn").forEach(btn => {
  const card = btn.closest(".service-card");
  btn.addEventListener("click", () => openLogs(card.dataset.name));
});

closeBtn.addEventListener("click", () => {
  closeLogs();
  modal.close();
});

clearBtn.addEventListener("click", () => {
  terminal.innerHTML = "";
});

/* Close on backdrop click */
modal.addEventListener("click", (e) => {
  if (e.target === modal) {
    closeLogs();
    modal.close();
  }
});

/* Close on Escape (dialog already handles this, but we need to stop the stream) */
modal.addEventListener("cancel", () => closeLogs());
