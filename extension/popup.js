const $ = (id) => document.getElementById(id);

async function refresh() {
  const { userOffsetMs = 0 } = await chrome.storage.local.get('userOffsetMs');
  $('off').value = userOffsetMs;

  let st = null;
  try {
    const resp = await chrome.runtime.sendMessage({ type: 'poll-status' });
    st = resp?.ok ? resp.status : null;
  } catch (_e) {
    st = null;
  }

  if (st) {
    $('api').textContent = 'connected';
    $('active').textContent = st.delay ? 'yes' : 'no';
    $('lead').textContent = `${st.lead_ms} ms`;
    $('eff').textContent = st.delay ? `${st.lead_ms + userOffsetMs} ms` : '0 ms';
  } else {
    $('api').textContent = 'unreachable';
    $('active').textContent = '-';
    $('lead').textContent = '-';
    $('eff').textContent = '0 ms';
  }
}

$('save').addEventListener('click', async () => {
  await chrome.storage.local.set({ userOffsetMs: Number($('off').value) || 0 });
  refresh();
});

refresh();
