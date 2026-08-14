// Fetch proxy for the loopback API.
//
// Content scripts on https pages cannot fetch http://127.0.0.1 directly
// (MV3 restricts content-script requests to what the host page may do,
// and Private Network Access would require the server to answer a CORS
// preflight). The service worker fetches with extension privileges;
// host_permissions covers 127.0.0.1:17653.

const API = 'http://127.0.0.1:17653/status';

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.type !== 'poll-status') return undefined;
  fetch(API, { cache: 'no-store' })
    .then((r) => (r.ok ? r.json() : null))
    .then((status) => sendResponse({ ok: status !== null, status }))
    .catch(() => sendResponse({ ok: false, status: null }));
  return true; // async sendResponse
});
