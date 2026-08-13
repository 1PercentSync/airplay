# 研究笔记 06：真机探测事实（HomePod gen 2，192.168.1.12）

> 设备能力以真机 `GET /info` 为准。2026-08-12 的可播放迭代属于**已删除实现**（git `a2898cb`）。更早一份探针因未对每一特性读全参考被删（勿通过 git 回看 `545e2d8`）。下列设备事实以 2026-08-13 重写探针的 Windows 真机跑分为准。

## 2026-08-13 重写探针（当前 `crates/`）

用户在 Windows 构建 `target\release\airplay.exe`，对 `192.168.1.12` 跑探针。2026-08-13 14:50 跑完 `devices`/`discover`/`airplay`/`pair`。15:02 再跑一遍，并加上 `probe channel`。成功行如下。

| 命令 | 成功行 | 证实了什么 |
|---|---|---|
| `probe devices` | `[STATUS] devices_ok count=3` | WASAPI 枚举与 `GetMixFormat` 可用 |
| `probe discover` | `[STATUS] discover_ok count=1` | 浏览 `_airplay._tcp` 5 秒，扫到同一台 HomePod |
| `probe airplay 192.168.1.12` | `[STATUS] info_ok` | 明文 `GET /info` 返回 200，body 2009 字节，bplist 可解析 |
| `probe pair 192.168.1.12` | `[STATUS] pair_ok` | transient M1–M4，HAMK 通过，`session_key_len=64` |
| `probe channel 192.168.1.12`（15:02 日志） | `[STATUS] channel_ok` | 同一条 TCP 上配对后加密 `GET /info` 返回 200，body 2009 字节，与明文 `/info` 一致 |

`probe discover`：名称 HomePod For Alex；Host `HomePod-For-Alex.local.`；端口 7000；IPv4 `192.168.1.12`（另有两条 ULA IPv6 与一条 `fe80` 链路本地）；`Use : 192.168.1.12:7000`；TXT `model=AudioAccessory6,1`、`deviceid=8E:35:21:D6:F9:74`、`features=0x4A7FCA00,0x3C354BD0`、`srcvers=980.71.1`、`protovers=1.1`、`osvers=27.0`。与手动 IP 和 `/info` 一致。

配对日志：M1 发送 State=1, Method=0, Flags=16；M2 收到 State=2, Salt=16B, PublicKey=384B；M3 发送 State=3, PublicKey=384B, Proof=64B；M4 收到 State=4, Proof=64B；HAMK ok。未发 `/pair-pin-start`。

`/info` 中 `supportedFormats.bufferStream` 打印为 `-577021992844656640 (0xf7fe018e00e80000)`（64 位位型十六进制，未按 i128 符号扩展）。`audioStream` 十进制 21235712 = `0x1440800`。`features` = `0x3c354bd04a7fca00`。`statusFlags` 十进制 623620 = `0x98404`。`senderAddress` = `192.168.1.100:52459`。

15:02 日志 `logs/probe-20260813-150235.log`：`failed_steps=0`。`probe channel` 先完成 M1–M4（HAMK ok），再打印 `encrypted RTSP 200 OK, body 2009 bytes` 和 `[STATUS] channel_ok`。这证实本机 HomePod 接受同连接控制通道加密。构建用的是工作区未提交代码（日志里 `git=f30cc35` 只是当时 HEAD）。

**还没测到的**：session/stream SETUP、RECORD、NTP、出声。

设备能力与声卡表与此前一致（见下）。

## 设备事实

| 项 | 值 | 含义/影响 |
|---|---|---|
| model | AudioAccessory6,1 | HomePod gen 2 |
| deviceid（mDNS TXT） | 8E:35:21:D6:F9:74 | 2026-08-13 `probe discover` |
| sourceVersion | 980.71.1（osBuild 24J5325d；TXT osvers=27.0） | 现代 audioOS |
| protocolVersion | "1.1" | AP2 音频（RAOP-over-AP2）路线适用 |
| features | 0x3C354BD04A7FCA00 | bit9 Audio、bit18-21 AudioFormats、bit38 AP2、bit40 Buffered、bit41 PTP、bit46 HKPairing、bit48 UnifiedScreen |
| statusFlags | 0x98404 | bit2（PinRequired 标注位）置位 + pk 存在——**但 transient 仍全流程成功**：statusFlags/pk 不阻止 transient pair-setup（该 HomePod 访问控制=所有人）。推翻了"现代 HomePod 上 transient 不可用"的担忧 |
| **transient 配对** | **pair_ok（HAMK 校验通过）** | 决策 E 主线成立；PIN pair-setup 应急预案继续封存 |
| supportedFormats.audioStream | 0x1440800 | realtime 广告仅含 0x40000（44.1/16/2）= 我们四格式中唯一；48k/24bit **未广告** → 决策 C 基线 44.1/16/2 被设备确认，hi-res 对 realtime 不适用 |
| supportedAudioFormatsExtended | 仅 bufferStream（34 项 codec 枚举） | 与 buffered 放弃决策一致，不利用 |
| keepAliveSendStatsAsBody | true | 设备广告支持统计 body；但 pyatv/owntone 均以**空 body** /feedback 真机服役 `[代码: pyatv support/rtsp.py:246; owntone airplay.c:3701]` → 空 body 起步，统计 body 为可选增强 |
| initialVolume | -26.25 dB | 接收端初始音量 |
| volumeControlType | 3 | SET_PARAMETER 绝对音量可用 |
| senderAddress | 192.168.1.100:52459 | 用户 PC 与 HomePod 同子网（端口为本次 `/info` 连接的本地口） |

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

## 2026-08-12 已删除实现的可播放迭代（不是本份代码）

下列两条是已删除实现（git `a2898cb`）在里程碑②上的真机记录。协议结论（IKM=64B、计时须先于流 SETUP）仍有效；那份代码本身已不在仓库。

### 迭代 #1：HKDF IKM 宽度 bug（已修）

**现象**：`run` 配对成功后第一个加密 RTSP 请求（GET /info）即被接收端断开（`early eof`）。

**根因**：`derive_keys` 曾将 64 字节 SRP K 截断为 32 字节作为 HKDF IKM。

**证据会审（清理后）**：
- owntone `airplay.c:1443`（注释明示 transient 密钥 64B）+ `airplay_events.c:150`：控制与事件通道 IKM 均为完整 64B；`shk` = K[..32]
- pyatv `hap_transient.py encryption_keys`：控制+事件均用完整 64B
- 真机实证：修正后加密 GET /info 成功（见上）
- （原第三方佐证材料 airplay-cli 已作为不可信移除，结论由两家 A 级 + 真机充分支撑）

**修正**：控制+事件密钥 IKM = 完整 64B SRP K；audio_key = K[..32]。mock 集成测试已同步对齐真机约定。

---

### 迭代 #2：计时服务须在流 SETUP 前就绪（已修，但非卡点根因）

**现象**：加密 GET /info ✓ → session SETUP ✓（event_port 下发）→ RECORD ✓ → **流 SETUP 无响应**（接收端沉默）；随后的重连在 M1 短暂被拒（HomePod 清理悬挂会话，数秒后自愈）。

**根因（部分）**：宣告 `timingProtocol: NTP` + timingPort 后，接收端在流 SETUP 前后即需要与发送端完成时钟同步；我们的 NTP timing server 原在 establish() 完成后才启动。

**证据**：owntone 计时服务常驻、先于一切会话启动 `[代码: owntone airplay.c:4314 service_start]`。

**修正**：`establish()` 绑定 timing socket 后立即 spawn timing server。

**结果（诚实记录）**：**修复后真机重测仍为流 SETUP 无响应**——该修正是必要非充分条件；卡点根因未定位，实验队列见 `docs/协议实现规范.md` §14。
