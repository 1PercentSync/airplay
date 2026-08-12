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
| keepAliveSendStatsAsBody | true | **保活要求**：/feedback 需携带统计 body（plist）——会话层实现时按 airplay-cli 的 keepalive-stats 格式执行 |
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
- 保活：POST /feedback ~2s + 统计 body（keepAliveSendStatsAsBody=true）+ 事件通道全程服务；
- SETUP 音量：初始 SET_PARAMETER 可按 initialVolume 附近设置。

---

## 2026-08-12 里程碑②真机迭代 #1：HKDF IKM 宽度 bug（已修）

**现象**：`run` 配对成功后第一个加密 RTSP 请求（GET /info）即被接收端断开（`early eof`）。

**根因**：`derive_keys` 曾将 64 字节 SRP K 截断为 32 字节作为 HKDF IKM。

**证据三方会审**：
- airplay-cli `ap2_hap.c:1172-1180`：控制通道 HKDF IKM = **完整 64B**（注释 "matching pair_ap/owntone"）
- owntone `airplay.c:1443` + `airplay_events.c:150`：控制与事件通道 IKM 均为完整 64B；`shk` = K[..32]
- pyatv `hap_transient.py encryption_keys`：控制+事件均用完整 64B
- 孤例：airplay-cli MRP 侧车（`ap2_mrp.c:1409`）事件通道用 32B——与自家 ap2_hap.c 不一致，疑为其 transient+MRP 路径潜伏 bug

**修正**：控制+事件密钥 IKM = 完整 64B SRP K；audio_key = K[..32]。mock 集成测试已同步对齐真机约定。

---

## 2026-08-12 里程碑②真机迭代 #2：计时服务必须在流 SETUP 前就绪（已修）

**现象**：加密 GET /info ✓ → session SETUP ✓（event_port 下发）→ RECORD ✓ → **流 SETUP 无响应**（接收端沉默）；随后的重连在 M1 短暂被拒（HomePod 清理悬挂会话，数秒后自愈）。

**根因**：宣告 `timingProtocol: NTP` + timingPort 后，接收端在流 SETUP 前后即需要与发送端完成时钟同步；我们的 NTP timing server 原在 establish() 完成后才启动 → 死锁。

**证据**：airplay-cli `ap2_client.c:1790-1811`（timing service 在 session SETUP 之前启动）；FXChainPlayer STARTPLAYING 序列（firstSync 先于 sendSessionSetup）；pyatv 用 `timingProtocol: None` 故无此约束（我们不采用 None——实时流需要时间锚定）。

**修正**：`establish()` 绑定 timing socket 后立即 spawn timing server；AbortOnDrop 守卫保证 establish 失败路径不泄漏任务；`Session.timing_replies` 计数器供诊断；run/mock 测试去掉了重复启动。

**次要观察**：establish 失败遗留的悬挂会话会让 HomePod 在数秒内拒绝新配对（M1 超时），退避重连可自愈——暂不加显式 TEARDOWN。
