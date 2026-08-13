# 研究笔记 06：真机探测事实（HomePod gen 2，192.168.1.12）

> 2026-08-12，用户真机执行 `probe` 三件套的全部结论。后续会话层实现的参数以此为准。

## 设备事实

| 项 | 值 | 含义/影响 |
|---|---|---|
| model | AudioAccessory6,1 | HomePod gen 2 |
| sourceVersion | 980.71.1（osBuild 24J5325d） | 现代 audioOS |
| protocolVersion | "1.1" | AP2 音频（RAOP-over-AP2）路线适用 |
| features | 0x3C354BD04A7FCA00 | bit9 Audio、bit18-21 AudioFormats、bit38 AP2、bit40 Buffered、bit41 PTP、bit46 HKPairing、bit48 UnifiedScreen |
| statusFlags | 0x98404 | bit2（PinRequired 标注位）置位 + pk 存在——**但 transient 仍全流程成功**：statusFlags/pk 不阻止 transient pair-setup（该 HomePod 访问控制=所有人）。推翻了"现代 HomePod 上 transient 不可用"的担忧 |
| **transient 配对** | **pair_ok（HAMK 校验通过）** | 决策 E 主线成立；PIN pair-setup 应急预案继续封存 |
| supportedFormats.audioStream | 0x1440800 | realtime 广告仅含 0x40000（44.1/16/2）= 我们四格式中唯一；48k/24bit **未广告** → 决策 C 基线 44.1/16/2 被设备确认，hi-res 对 realtime 不适用 |
| supportedAudioFormatsExtended | 仅 bufferStream（34 项 codec 枚举） | 与 buffered 放弃决策一致，不利用 |
| keepAliveSendStatsAsBody | true | 设备广告支持统计 body；但 pyatv/owntone 均以**空 body** /feedback 真机服役 `[代码: pyatv support/rtsp.py:246; owntone airplay.c:3701]` → 空 body 起步，统计 body 为可选增强 |
| initialVolume | -26.25 dB | 接收端初始音量 |
| volumeControlType | 3 | SET_PARAMETER 绝对音量可用 |
| senderAddress | 192.168.1.100 | 用户 PC 与 HomePod 同子网 |

## 声卡事实（probe devices）

| 端点 | 混合格式 | 结论 |
|---|---|---|
| 扬声器 (Steam Streaming Speakers) | 48000Hz 2ch 32bit float（valid_bits=32） | 与 VB-CABLE 官方参数一致；48k 直通零 SRC，44.1k 走 rubato。**默认采集设备确认** |
| Realtek HD Audio 2nd output | 192000Hz 2ch 32bit float | — |
| LG ULTRAGEAR (NVIDIA HDMI) | 48000Hz 2ch 32bit float | — |

## 对会话层的参数定案

- 流格式：ALAC 44.1kHz/16bit/2ch，spf=352（唯一广告 realtime 格式）；
- 计时：NTP 先行（PTP 支持在位但按决策备用）；
- 保活：POST /feedback ~2s **空 body**（统计 body 为可选增强，见上表）+ 事件通道全程服务；
- SETUP 音量：初始 SET_PARAMETER 可按 initialVolume 附近设置。

---

## 2026-08-12 里程碑②真机迭代 #1：HKDF IKM 宽度 bug（已修）

**现象**：`run` 配对成功后第一个加密 RTSP 请求（GET /info）即被接收端断开（`early eof`）。

**根因**：`derive_keys` 曾将 64 字节 SRP K 截断为 32 字节作为 HKDF IKM。

**证据会审（清理后）**：
- owntone `airplay.c:1443`（注释明示 transient 密钥 64B）+ `airplay_events.c:150`：控制与事件通道 IKM 均为完整 64B；`shk` = K[..32]
- pyatv `hap_transient.py encryption_keys`：控制+事件均用完整 64B
- 真机实证：修正后加密 GET /info 成功（见上）
- （原第三方佐证材料 airplay-cli 已作为不可信移除，结论由两家 A 级 + 真机充分支撑）

**修正**：控制+事件密钥 IKM = 完整 64B SRP K；audio_key = K[..32]。mock 集成测试已同步对齐真机约定。

---

## 2026-08-12 里程碑②真机迭代 #2：计时服务须在流 SETUP 前就绪（已修，但非卡点根因）

**现象**：加密 GET /info ✓ → session SETUP ✓（event_port 下发）→ RECORD ✓ → **流 SETUP 无响应**（接收端沉默）；随后的重连在 M1 短暂被拒（HomePod 清理悬挂会话，数秒后自愈）。

**根因（部分）**：宣告 `timingProtocol: NTP` + timingPort 后，接收端在流 SETUP 前后即需要与发送端完成时钟同步；我们的 NTP timing server 原在 establish() 完成后才启动。

**证据**：owntone 计时服务常驻、先于一切会话启动 `[代码: owntone airplay.c:4314 service_start]`。

**修正**：`establish()` 绑定 timing socket 后立即 spawn timing server。

**结果（诚实记录）**：**修复后真机重测仍为流 SETUP 无响应**——该修正是必要非充分条件；卡点根因未定位，实验队列见 `docs/协议实现规范.md` §14。
