// MAIN world hook: shift the video SourceBuffer timeline so video frames
// render `delaySec` later than audio. Verified against bilibili's DASH
// player (single <video>, one MediaSource, separate video/audio
// SourceBuffers; the player never writes timestampOffset itself, and it
// rebuilds the video SourceBuffer on quality change — our addSourceBuffer
// hook re-applies the offset there).
//
// Seek behaviour (measured): no freeze with small offsets; when seeking
// back to 0, Chromium aligns audio to the first video frame, so the first
// `delaySec` of audio is skipped. Negligible at real offsets (~0.12 s).

(() => {
  const MSG_SOURCE = 'airplay-av-sync';
  let delaySec = 0;
  const videoSBs = new Set();

  const offsetDesc = Object.getOwnPropertyDescriptor(
    SourceBuffer.prototype,
    'timestampOffset'
  );

  function apply(sb) {
    if (sb.updating) {
      // Setting timestampOffset during an append throws; retry after it.
      sb.addEventListener('updateend', () => apply(sb), { once: true });
      return;
    }
    try {
      offsetDesc.set.call(sb, delaySec);
    } catch (_e) {
      videoSBs.delete(sb); // detached from its MediaSource
    }
  }

  const origAdd = MediaSource.prototype.addSourceBuffer;
  MediaSource.prototype.addSourceBuffer = function (mime) {
    const sb = origAdd.apply(this, arguments);
    if (String(mime).startsWith('video/')) {
      videoSBs.add(sb);
      apply(sb);
    }
    return sb;
  };

  const origRemove = MediaSource.prototype.removeSourceBuffer;
  MediaSource.prototype.removeSourceBuffer = function (sb) {
    videoSBs.delete(sb);
    return origRemove.apply(this, arguments);
  };

  window.addEventListener('message', (ev) => {
    const d = ev.data;
    if (!d || d.source !== MSG_SOURCE || typeof d.delaySec !== 'number') return;
    if (d.delaySec === delaySec) return;
    delaySec = d.delaySec;
    videoSBs.forEach(apply);
  });
})();
