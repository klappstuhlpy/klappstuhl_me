document.addEventListener("DOMContentLoaded", () => {
    document.querySelectorAll(".service-card").forEach(card => {
        const isRunning = card.dataset.running === "true";
        const startBtn = card.querySelector(".start-btn");
        const stopBtn = card.querySelector(".stop-btn");

        if (isRunning) {
            startBtn.disabled = true;
            stopBtn.disabled = false;
        } else {
            startBtn.disabled = false;
            stopBtn.disabled = true;
        }
    });
});