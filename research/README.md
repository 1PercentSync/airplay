# 研究进度总览

> 本文件是当前状态的索引与进度追踪。详细内容见各笔记；决策历史见 `02-协议调研.md` §7。

## 项目阶段

| 阶段 | 状态 | 产出 |
|---|---|---|
| 需求冻结 | ✅ | `REQUIREMENTS.zh-CN.md`（冻结，未经授权禁改） |
| 阶段一：协议调研（两轮） | ✅ | `02-协议调研.md`、`03-协议调研-第二轮.md` |
| 阶段二：音频捕获调研 | ✅ | `04-音频捕获.md` |
| 技术选型（Context7 验证） | ✅ | `05-技术选型.md` |
| 架构设计文档 | ✅ | `docs/架构设计.md` |
| 里程碑 ① 探针 CLI | ✅（真机验证通过） | transient 配对/能力探测全过，见 `06-真机探测-HomePod.md` |
| 里程碑 ② 可播放 CLI | ⏳ 代码完成，待真机验证 | 全链路（配对→加密通道→会话→事件→RECORD→流→NTP/sync→RTP/ALAC→采集管线→断流恢复）；29 测试全过含端到端 mock 会话；WASAPI 部分未编译验证 |
| 里程碑 ② 可播放 CLI | ⏳ | **用户测试节点** |
| 里程碑 ③ 托盘 GUI | ⏳ | — |

## 决策快照（完整历史见 02 §7）

| # | 结论 |
|---|---|
| A | Rust + 规范驱动自研；依赖白名单；禁 airplay2-rs/AirSend；NTP 先行 |
| B | realtime (type 0x60) 唯一路线 |
| C | 44.1/16/2 保底；hi-res 按设备广告启用（探针定），基线后再开 |
| D | 第三方虚拟声卡（默认实测 Steam Streaming Speakers，异常→VB-CABLE） |
| E | transient 无密码；PIN pair-setup 应急预案（触发=真机失败）；设备密码不支持 |
| 实施默认 | 探针先行；延迟窗 250ms~2s；mDNS+手动 IP；断流自动恢复+splice 静音纪律；单 exe+日志交付 |

## 参考资产

- 17 个参考仓库克隆于 `reference/repos/`（SHA 钉死），清单：`references/REFERENCES.md`
- 协议文档/网页已落盘：`references/`（airplay2-internals 全站、pyatv 协议页、MA provider README、Reddit 帖）
- 检索原始记录：`references/search_*.json`

## 协议发送链路（实现蓝本，详见 02 §3 + 03）

1. mDNS 发现（`_airplay._tcp`）/ 手动 IP → 2. 明文 GET /info（能力探测）→ 3. transient pair-setup（HKP 4，PIN 3939，SRP-6a/SHA-512/3072，M1-M4）→ 4. 加密 RTSP（ChaCha20-Poly1305 帧）→ 5. session SETUP（NTP timingPort）→ 6. 事件通道（反向 TCP，全程服务）→ 7. RECORD → 8. stream SETUP（type 96，ALAC，spf 352，shk=秘密前 32B）→ 9. NTP 计时应答 + 1s sync → 10. ALAC RTP 加密发送（44100/s pacing，重传 backlog）→ 保活（2s feedback + 事件服务）；断流→新连接重配。
