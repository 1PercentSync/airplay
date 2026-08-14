// MAIN world hook: shift the video SourceBuffer timeline so video frames
// render `wallDelaySec` later than audio, in wall-clock time.
//
// timestampOffset lives on the MEDIA timeline, which flows at
// playbackRate × wall clock, while the HomePod lead is wall-clock.
// So the offset must be scaled by the current playback rate:
//
//     timestampOffset = wallDelaySec × playbackRate
//
// Verified against bilibili's DASH player (single <video>, one
// MediaSource, separate video/audio SourceBuffers; the player never
// writes timestampOffset itself, and it rebuilds the video SourceBuffer
// on quality change — our addSourceBuffer hook re-applies the offset
// there). Rate changes (0.5x–2x, both directions): offset re-applied,
// no stalls, no seeks, no buffer surgery. Measured at wallDelay 0.5 s;
// at real offsets (~0.12 s) every edge effect is below perception.
//
// We deliberately do NOT remove() buffered video on rate change: the
// bilibili player never re-appends already-consumed ranges, so removing
// creates a gap that Chromium gap-jumps (audio skips too). At real
// offsets the stale-offset tail is ≤ ~60 ms of error — inaudible.
//
// Seeking back to 0 skips the first `offset` of audio (Chromium aligns
// audio to the first video frame) — negligible at real offsets.

(() => {
  const MSG_SOURCE = 'airplay-av-sync';
  let wallDelaySec = 0; // wall-clock seconds, from content.js
  const videoSBs = new Set();

  const offsetDesc = Object.getOwnPropertyDescriptor(
    SourceBuffer.prototype,
    'timestampOffset'
  );

  function currentRate() {
    const v = document.querySelector('video');
    return v ? v.playbackRate : 1;
  }

  function effectiveOffset() {
    return wallDelaySec * currentRate();
  }

  function apply(sb) {
    if (sb.updating) {
      // Setting timestampOffset during an append throws; retry after it.
      sb.addEventListener('updateend', () => apply(sb), { once: true });
      return;
    }
    try {
      offsetDesc.set.call(sb, effectiveOffset());
    } catch (_e) {
      videoSBs.delete(sb); // detached from its MediaSource
    }
  }

  function applyAll() {
    videoSBs.forEach(apply);
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

  // The <video> element appears after document_start; watch for it and
  // re-apply the offset whenever the playback rate changes.
  function hookVideo() {
    const v = document.querySelector('video');
    if (!v || v.__avSyncHooked) return;
    v.__avSyncHooked = true;
    v.addEventListener('ratechange', applyAll);
  }
  function boot() {
    try {
      new MutationObserver(hookVideo).observe(document.documentElement || document, {
        childList: true,
        subtree: true,
      });
    } catch (_e) {}
    hookVideo();
  }
  if (document.documentElement) boot();
  else document.addEventListener('readystatechange', boot, { once: true });

  window.addEventListener('message', (ev) => {
    const d = ev.data;
    if (!d || d.source !== MSG_SOURCE || typeof d.delaySec !== 'number') return;
    wallDelaySec = d.delaySec;
    // Idempotent and cheap; also repairs any external overwrite of the
    // offset within one poll interval.
    applyAll();
  });
})();
