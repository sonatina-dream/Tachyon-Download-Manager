const DEFAULTS = {
  port: 7311,
  takeOver: true,
  extensions:
    "zip rar 7z gz tgz bz2 xz tar dmg pkg iso img exe msi apk deb rpm " +
    "mp4 m4v mkv mov avi wmv webm flv mp3 m4a flac wav aac ogg pdf epub",
};

const $ = (id) => document.getElementById(id);

async function load() {
  const s = { ...DEFAULTS, ...(await chrome.storage.local.get(Object.keys(DEFAULTS))) };
  $("takeOver").checked = s.takeOver;
  $("extensions").value = s.extensions;
  $("port").value = s.port;
  refreshStatus();
}

function refreshStatus() {
  chrome.runtime.sendMessage({ type: "status" }, (reply) => {
    const status = $("status");
    const connected = Boolean(reply?.connected);
    status.textContent = connected ? "● Connected" : "● Not running";
    status.style.color = connected ? "var(--ok)" : "var(--bad)";
  });
}

$("takeOver").addEventListener("change", (e) => chrome.storage.local.set({ takeOver: e.target.checked }));
$("extensions").addEventListener("change", (e) => chrome.storage.local.set({ extensions: e.target.value }));
$("port").addEventListener("change", (e) => {
  const port = Number(e.target.value);
  if (port >= 1024 && port <= 65535) {
    chrome.storage.local.set({ port }).then(refreshStatus);
  }
});

load();
