// ISOLATED world: polls the local airplay API (via the background fetch
// proxy) once per second and forwards the effective delay to the
// MAIN-world hook in inject.js via window.postMessage.
//
// Effective delay = lead_ms + user offset, only while the API says
// `delay` (streaming to HomePod AND a browser session is active on the
// capture endpoint). Otherwise 0.

const MSG_SOURCE = 'airplay-av-sync';

async function poll() {
  let delaySec = 0;
  try {
    const resp = await chrome.runtime.sendMessage({ type: 'poll-status' });
    const st = resp?.ok ? resp.status : null;
    if (st && st.delay === true && typeof st.lead_ms === 'number') {
      const { userOffsetMs = 0 } = await chrome.storage.local.get('userOffsetMs');
      delaySec = st.lead_ms / 1000 + userOffsetMs / 1000;
    }
  } catch (_e) {
    // Service worker hiccup or extension reload: keep 0.
  }
  window.postMessage({ source: MSG_SOURCE, delaySec }, '*');
}

setInterval(poll, 1000);
poll();
