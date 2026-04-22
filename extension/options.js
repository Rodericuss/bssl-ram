// bssl-ram signals — options page
//
// Pure read-only dashboard. Talks to the daemon exclusively through
// the `bssl-ram-bridge` native messaging host — never opens any
// network socket of its own. If the bridge / daemon is down, the
// status card shows it in red and the page stays functional for
// everything else.

const api = globalThis.browser ?? globalThis.chrome;

const NMH_HOST = "io.bssl.ram";
const EXPECTED_PROTOCOL_VERSION = 1;
const STORAGE_KEY_INSTANCE_ID = "instanceId";
const STORAGE_KEY_LAST_FAILURE_AT = "lastFailureAt";

async function detectFamily() {
  try {
    if (typeof api.runtime.getBrowserInfo === "function") {
      const info = await api.runtime.getBrowserInfo();
      return `${info?.name ?? "?"} ${info?.version ?? ""}`.trim();
    }
  } catch (_) {
    /* fall through */
  }
  return navigator.userAgent;
}

async function loadInstanceId() {
  const stored = await api.storage.local.get(STORAGE_KEY_INSTANCE_ID);
  return stored?.[STORAGE_KEY_INSTANCE_ID] ?? "(not yet assigned)";
}

async function loadLastFailure() {
  const stored = await api.storage.session.get(STORAGE_KEY_LAST_FAILURE_AT);
  const ts = stored?.[STORAGE_KEY_LAST_FAILURE_AT];
  if (!Number.isFinite(ts) || ts <= 0) return "never";
  const ageSec = Math.max(0, Math.round((Date.now() - ts) / 1000));
  return `${new Date(ts).toLocaleString()} (${ageSec}s ago)`;
}

function setStatus(el, kind, text) {
  el.className = `status ${kind}`;
  el.innerHTML = "";
  const dot = document.createElement("span");
  dot.className = "dot";
  el.append(dot, document.createTextNode(text));
}

/**
 * One-shot NMH ping. We open a fresh port, send `{kind:"ping"}`, wait
 * for the single reply, then disconnect. Avoids tangling with the
 * background SW's long-lived port.
 */
function pingViaBridge() {
  return new Promise((resolve, reject) => {
    let port;
    try {
      port = api.runtime.connectNative(NMH_HOST);
    } catch (err) {
      reject(err);
      return;
    }
    const timer = setTimeout(() => {
      try { port.disconnect(); } catch (_) {}
      reject(new Error("bridge timed out"));
    }, 3_000);
    port.onMessage.addListener((msg) => {
      clearTimeout(timer);
      try { port.disconnect(); } catch (_) {}
      resolve(msg);
    });
    port.onDisconnect.addListener(() => {
      clearTimeout(timer);
      const err = api.runtime.lastError?.message ?? "bridge disconnected";
      reject(new Error(err));
    });
    port.postMessage({ kind: "ping" });
  });
}

async function checkDaemon() {
  const status = document.getElementById("daemon-status");
  const proto = document.getElementById("daemon-protocol");
  const fams = document.getElementById("daemon-families");
  const lim = document.getElementById("daemon-limit");
  const endpointEl = document.getElementById("daemon-endpoint");
  const bridgeEl = document.getElementById("daemon-bridge");

  setStatus(status, "", "checking…");
  proto.textContent = "—";
  fams.textContent = "—";
  lim.textContent = "—";
  endpointEl.textContent = "—";
  if (bridgeEl) bridgeEl.textContent = "—";

  try {
    const body = await pingViaBridge();

    if (body?.ok === false) {
      setStatus(status, "err", body.reason ?? "bridge error");
      return;
    }

    const protocol = Number(body?.protocol_version);
    const families = Array.isArray(body?.accepted_families)
      ? body.accepted_families.join(", ")
      : "?";
    const limit = Number.isFinite(body?.max_report_bytes)
      ? `${Math.round(body.max_report_bytes / 1024)} KiB`
      : "?";

    endpointEl.textContent = body?.bridge_kind ?? "native-messaging";
    proto.textContent = `v${protocol}`;
    fams.textContent = families;
    lim.textContent = limit;
    if (bridgeEl) {
      bridgeEl.textContent = body?.bridge_version ?? "—";
    }

    if (protocol === EXPECTED_PROTOCOL_VERSION) {
      setStatus(status, "ok", "reachable");
    } else {
      setStatus(
        status,
        "warn",
        `protocol mismatch — extension expects v${EXPECTED_PROTOCOL_VERSION}`
      );
    }
  } catch (err) {
    setStatus(status, "err", err.message ?? "bridge unavailable");
  }
}

async function hasRichPermission() {
  try {
    return await api.permissions.contains({ origins: ["<all_urls>"] });
  } catch (_) {
    return false;
  }
}

async function refreshRichStatus() {
  const granted = await hasRichPermission();
  const status = document.getElementById("rich-status");
  const btn = document.getElementById("rich-toggle");
  if (granted) {
    status.textContent = "enabled — content script registered on all URLs";
    btn.textContent = "Disable";
  } else {
    status.textContent = "disabled — only coarse signals are reported";
    btn.textContent = "Enable";
  }
}

async function toggleRichPermission() {
  const granted = await hasRichPermission();
  try {
    if (granted) {
      await api.permissions.remove({ origins: ["<all_urls>"] });
    } else {
      await api.permissions.request({ origins: ["<all_urls>"] });
    }
  } catch (err) {
    console.warn("permission toggle failed", err);
  }
  await refreshRichStatus();
}

async function refresh() {
  const manifest = api.runtime.getManifest();
  document.getElementById("ext-version").textContent = manifest.version;
  document.getElementById("ext-family").textContent = await detectFamily();
  document.getElementById("ext-instance").textContent = await loadInstanceId();
  document.getElementById("ext-last-failure").textContent = await loadLastFailure();
  await checkDaemon();
  await refreshRichStatus();
}

document.getElementById("refresh").addEventListener("click", () => {
  void refresh();
});
document.getElementById("rich-toggle").addEventListener("click", () => {
  void toggleRichPermission();
});

void refresh();
