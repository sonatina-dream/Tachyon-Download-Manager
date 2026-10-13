// Hands links and browser downloads over to the Tachyon app via its local endpoint
// (127.0.0.1 only). The app shows its "Download File Info" dialog before anything
// is downloaded, unless the user opted out in Tachyon's Options.

const DEFAULTS = {
  port: 7311,
  takeOver: true,
  // Downloads with these extensions are taken over from the browser.
  extensions:
    "zip rar 7z gz tgz bz2 xz tar dmg pkg iso img exe msi apk deb rpm " +
    "mp4 m4v mkv mov avi wmv webm flv mp3 m4a flac wav aac ogg pdf epub",
};

const MENU_ID = "tachyon-download";
// URLs we handed back to the browser after Tachyon was unreachable.
const passThrough = new Set();

async function settings() {
  return { ...DEFAULTS, ...(await chrome.storage.local.get(Object.keys(DEFAULTS))) };
}

function endpoint(port, path) {
  return `http://127.0.0.1:${port}${path}`;
}

async function ping() {
  const { port } = await settings();
  try {
    const response = await fetch(endpoint(port, "/v1/ping"), {
      headers: { "X-Tachyon-Client": "extension" },
      signal: AbortSignal.timeout(1500),
    });
    return response.ok;
  } catch {
    return false;
  }
}

async function cookieHeader(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

/** Sends one download to Tachyon; resolves to true if the app accepted it. */
async function sendToTachyon({ url, referrer, filename }) {
  if (!/^https?:/i.test(url)) return false;
  const { port } = await settings();
  const body = {
    url,
    referrer: referrer || "",
    filename: filename || "",
    cookies: await cookieHeader(url),
    userAgent: navigator.userAgent,
  };
  try {
    const response = await fetch(endpoint(port, "/v1/download"), {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-Tachyon-Client": "extension" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(3000),
    });
    return response.ok;
  } catch {
    return false;
  }
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: MENU_ID,
    title: "Download with Tachyon",
    contexts: ["link", "image", "video", "audio"],
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== MENU_ID) return;
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  const ok = await sendToTachyon({ url, referrer: info.pageUrl || tab?.url });
  if (!ok) {
    // Tachyon isn't running: fall back to the browser.
    passThrough.add(url);
    chrome.downloads.download({ url });
  }
});

function extensionOf(item) {
  const name = (item.filename || "").split(/[\\/]/).pop() || "";
  const fromName = name.includes(".") ? name.split(".").pop() : "";
  if (fromName) return fromName.toLowerCase();
  try {
    const path = new URL(item.finalUrl || item.url).pathname;
    const last = path.split("/").pop() || "";
    return last.includes(".") ? last.split(".").pop().toLowerCase() : "";
  } catch {
    return "";
  }
}

// Take over matching downloads, like IDM's browser integration.
chrome.downloads.onCreated.addListener(async (item) => {
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url) || item.byExtensionId === chrome.runtime.id) return;
  if (passThrough.delete(url)) return;
  const { takeOver, extensions } = await settings();
  if (!takeOver) return;
  const wanted = new Set(extensions.toLowerCase().split(/[\s,]+/).filter(Boolean));
  if (!wanted.has(extensionOf(item))) return;
  if (!(await ping())) return;

  await chrome.downloads.cancel(item.id).catch(() => {});
  await chrome.downloads.erase({ id: item.id }).catch(() => {});
  const filename = (item.filename || "").split(/[\\/]/).pop();
  const ok = await sendToTachyon({ url, referrer: item.referrer, filename });
  if (!ok) {
    passThrough.add(url);
    chrome.downloads.download({ url });
  }
});

// Popup API.
chrome.runtime.onMessage.addListener((message, _sender, reply) => {
  if (message?.type === "status") {
    ping().then((connected) => reply({ connected }));
    return true;
  }
  return false;
});
