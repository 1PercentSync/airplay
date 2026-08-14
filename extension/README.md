# AirPlay A/V Sync (browser extension)

Delays bilibili video so the picture matches HomePod audio when the
`airplay` app is streaming. Chromium renders MSE video/audio by their own
timestamps; the extension shifts the video `SourceBuffer` timeline by
`timestampOffset = lead + user offset`. The audio track is untouched.

## How it works

- The airplay tray app serves `GET http://127.0.0.1:17653/status` →
  `{"delay": bool, "lead_ms": int}`. `delay` is true only while it is
  streaming to HomePod AND an Edge/Chrome session is active on the
  configured capture endpoint.
- `content.js` (isolated world) polls that endpoint once per second via
  `background.js` (content scripts cannot call loopback directly under
  Private Network Access; the service worker can).
- `inject.js` (MAIN world, document_start) hooks
  `MediaSource.prototype.addSourceBuffer`; every video `SourceBuffer`
  (including the ones bilibili recreates on quality change) gets
  `timestampOffset = (lead_ms + userOffsetMs) / 1000` while `delay` is
  true, else 0.

Measured on bilibili (AV1/AAC DASH): no stalls, no dropped frames, seeks
recover normally. Seeking back to 0 skips the first `delay` of audio
(Chromium aligns audio to the first video frame) — negligible at real
offsets (~120 ms).

## Install

1. Edge/Chrome → `edge://extensions` (or `chrome://extensions`) → enable
   **Developer mode**.
2. **Load unpacked** → select this `extension/` directory.
3. Start the `airplay` tray app (its API must listen on 127.0.0.1:17653).
4. Open any `https://www.bilibili.com/video/...` page. While streaming,
   video lags audio by `lead + offset`; when streaming stops, delay
   returns to 0.

## Settings (toolbar popup)

- **Extra offset**: milliseconds added on top of the AirPlay lead. Saved
  in `chrome.storage.local`, applied within one poll interval (1 s).
- The popup shows API reachability, whether delaying is active, the
  current lead, and the effective delay.

## Limits

- bilibili video pages only (`www.bilibili.com/video/*`).
- Process-level detection: the API cannot tell which tab produces the
  audio; `delay` reflects Edge/Chrome playing on the capture endpoint.
- Port is fixed at 17653 (matches `api.port` default in airplay.toml).
