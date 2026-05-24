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

/** Strip ANSI escape codes so raw Docker output renders cleanly. */
function stripAnsi(str) {
  // eslint-disable-next-line no-control-regex
  return str.replace(/\x1b\[[0-9;]*[a-zA-Z]/g, "");
}

function setConnectionState(state) {
  connDot.className   = "log-connection-dot " + state;    // connecting | connected | closed | error
  const labels = { connecting: "Connecting…", connected: "Connected", closed: "Closed", error: "Error" };
  connLabel.textContent = labels[state] ?? state;
}

function appendLine(text) {
  const line = document.createElement("div");
  line.className   = "log-line";
  line.textContent = stripAnsi(text);
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
