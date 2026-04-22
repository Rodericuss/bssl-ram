const SAMPLE_INTERVAL_MS = 15_000;

let lastUserInteractionAt = Date.now();

function mediaSnapshot() {
    const media = Array.from(document.querySelectorAll("audio, video"));
    let playing = 0;

    for (const element of media) {
        if (!element.paused && !element.ended && element.readyState > 2) {
            playing += 1;
        }
    }

    return {
        media_elements: media.length,
        playing_media_elements: playing
    };
}

function collectSignal() {
    const media = mediaSnapshot();
    return {
        kind: "bssl-content-signal",
        visibility_state: document.visibilityState ?? "unknown",
        document_has_focus: document.hasFocus(),
        fullscreen: Boolean(document.fullscreenElement),
        picture_in_picture: Boolean(document.pictureInPictureElement),
        media_elements: media.media_elements,
        playing_media_elements: media.playing_media_elements,
        last_user_interaction_ms: lastUserInteractionAt,
        sampled_at_ms: Date.now()
    };
}

function sendSignal() {
    try {
        const runtime = globalThis.browser?.runtime ?? globalThis.chrome?.runtime;
        runtime.sendMessage(collectSignal());
    } catch (_) {
        // The background context may not be ready yet. The next event/interval
        // will retry.
    }
}

function markInteraction() {
    lastUserInteractionAt = Date.now();
    sendSignal();
}

document.addEventListener("visibilitychange", sendSignal, {passive: true});
window.addEventListener("focus", sendSignal, {passive: true});
window.addEventListener("blur", sendSignal, {passive: true});
document.addEventListener("fullscreenchange", sendSignal, {passive: true});

for (const eventName of ["pointerdown", "keydown", "wheel", "touchstart"]) {
    window.addEventListener(eventName, markInteraction, {
        passive: true,
        capture: true
    });
}

for (const eventName of ["play", "pause", "ended", "volumechange"]) {
    document.addEventListener(eventName, sendSignal, {
        passive: true,
        capture: true
    });
}

setInterval(sendSignal, SAMPLE_INTERVAL_MS);
sendSignal();
