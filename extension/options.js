// bssl-ram signals — options page script
//
// Pure read-only dashboard: queries the daemon's `/v1/signals/ping`
// endpoint (same candidate endpoints the background SW uses), shows the
// extension's instance ID, and exposes the last transport failure.

const api = globalThis.browser ?? globalThis.chrome;

const PING_ENDPOINTS = [
  "http://127.0.0.1:7879/v1/signals/ping",
  "http://localhost:7879/v1/signals/ping",
  "http://[::1]:7879/v1/signals/ping"
];
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

async function checkDaemon() {
  const status = document.getElementById("daemon-status");
  const proto = document.getElementById("daemon-protocol");
  const fams = document.getElementById("daemon-families");
  const lim = document.getElementById("daemon-limit");
  const endpointEl = document.getElementById("daemon-endpoint");

  setStatus(status, "", "checking…");

  for (const endpoint of PING_ENDPOINTS) {
    try {
      const response = await fetch(endpoint, { method: "GET" });
      if (!response.ok) continue;
      const body = await response.json();
      const protocol = Number(body?.protocol_version);
      const families = Array.isArray(body?.accepted_families)
        ? body.accepted_families.join(", ")
        : "?";
      const limit = Number.isFinite(body?.max_report_bytes)
        ? `${Math.round(body.max_report_bytes / 1024)} KiB`
        : "?";

      endpointEl.textContent = endpoint;
      proto.textContent = `v${protocol}`;
      fams.textContent = families;
      lim.textContent = limit;

      if (protocol === EXPECTED_PROTOCOL_VERSION) {
        setStatus(status, "ok", "reachable");
      } else {
        setStatus(
          status,
          "warn",
          `protocol mismatch — extension expects v${EXPECTED_PROTOCOL_VERSION}`
        );
      }
      return;
    } catch (_) {
      /* try next */
    }
  }

  setStatus(status, "err", "daemon unreachable on all loopback endpoints");
  endpointEl.textContent = "—";
  proto.textContent = "—";
  fams.textContent = "—";
  lim.textContent = "—";
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
      // `permissions.request` must be called directly from a user
      // gesture (click handler) — anything async between the click and
      // the call will silently fail on Chromium. We keep the await
      // chain minimal above for that reason.
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
