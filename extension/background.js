const DAEMON_URL = "ws://127.0.0.1:7878";
const REPORT_INTERVAL_MS = 10_000; // report every 10s
const IDLE_THRESHOLD_SECS = 60;

// Tracks when each tab was last active (tab id → timestamp)
const lastActive = {};

let ws = null;
let reportInterval = null;

// ── Tab activity tracking ─────────────────────────────────────────────────────

browser.tabs.onActivated.addListener(({ tabId }) => {
  lastActive[tabId] = Date.now();
});

browser.tabs.onRemoved.addListener((tabId) => {
  delete lastActive[tabId];
});

// Initialise timestamps for tabs already open at startup
browser.tabs.query({}).then((tabs) => {
  const now = Date.now();
  for (const tab of tabs) {
    if (!(tab.id in lastActive)) {
      lastActive[tab.id] = now;
    }
  }
});

// ── Daemon communication ──────────────────────────────────────────────────────

function connect() {
  ws = new WebSocket(DAEMON_URL);

  ws.onopen = () => {
    console.log("[bssl-ram] connected to daemon");
    reportInterval = setInterval(sendReport, REPORT_INTERVAL_MS);
  };

  ws.onclose = () => {
    console.log("[bssl-ram] daemon disconnected — retrying in 5s");
    clearInterval(reportInterval);
    setTimeout(connect, 5000);
  };

  ws.onerror = (e) => console.error("[bssl-ram] WebSocket error:", e);
}

async function sendReport() {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;

  const now = Date.now();
  const allTabs = await browser.tabs.query({});

  // browser.processes gives us {processId → {id, type, tabs: [{tabId}]}}
  // We need the inverse: tabId → processId
  const processes = await browser.processes.getProcessInfo([], false);
  const tabToPid = {};
  for (const proc of Object.values(processes)) {
    if (proc.type !== "content") continue;
    for (const tab of (proc.tabs || [])) {
      tabToPid[tab.tabId] = proc.id; // proc.id is the OS PID in Firefox
    }
  }

  const activeTab = allTabs.find((t) => t.active);
  const activeTabId = activeTab?.id;

  const tabs = allTabs
    .filter((tab) => tabToPid[tab.id] !== undefined)
    .map((tab) => ({
      pid: tabToPid[tab.id],
      url: tab.url,
      active: tab.id === activeTabId,
      idle_seconds: Math.floor((now - (lastActive[tab.id] ?? now)) / 1000),
    }));

  ws.send(JSON.stringify({ tabs }));
}

connect();
