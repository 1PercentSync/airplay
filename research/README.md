# 研究进度总览

> 全部文档的作用说明与索引见根目录 `README.md`。本文件只追踪研究阶段状态；决策历史见 `archive/02-协议调研.md` §7。

## 项目阶段

| 阶段 | 状态 | 产出 |
|---|---|---|
| 需求冻结 | ✅ | `REQUIREMENTS.zh-CN.md`（冻结，未经授权禁改） |
| 阶段一：协议调研（两轮） | ✅（2026-08-12 依据清理+取证完成；档案入 `archive/`） | `archive/02`、`archive/03`、`archive/07`；**实现规范 = `docs/协议实现规范.md`** |
| 阶段二：音频捕获调研 | ✅ | `04-音频捕获.md` |
| 技术选型（Context7 验证） | ✅ | `05-技术选型.md` |
| 架构设计文档 | ✅ | `docs/架构设计.md` |
| 里程碑 ① 探针 CLI | ✅（真机验证通过） | transient 配对/能力探测全过，见 `06-真机探测-HomePod.md` |
| 里程碑 ② 可播放 CLI | ⏳ 待重写 | 首版实现已证实：①依据被不可信 AI 项目污染；②与研究文档不一致；③真机卡点（流 SETUP 无响应）未解。**代码已于 2026-08-12 全量删除**（git `a2898cb`，历史可溯 `937eca2`），待按清理后规范重写；真机事实见 `06` |
| 里程碑 ③ 托盘 GUI | ⏳ | — |

## 决策快照

> 权威版本：`docs/决策记录.md`（三权威文档之一，按会话原话重建）。下表为 Agent 便览缩写，如有出入以权威版本为准。

| # | 结论 |
|---|---|
| A | Rust + 规范驱动自研；依赖白名单；禁 airplay2-rs/AirSend（后被证实正确）；NTP 先行 |
| B | realtime (type 0x60) 唯一路线 |
| C | 44.1/16/2 落定（真机确认 realtime 仅此格式广告） |
| D | 第三方虚拟声卡（默认实测 Steam Streaming Speakers，异常→VB-CABLE） |
| E | transient 无密码（真机成功）；PIN pair-setup 预案封存；设备密码不支持 |
| 实施默认 | 探针先行；延迟窗 250ms~2s；mDNS+手动 IP；断流自动恢复+splice 静音纪律；单 exe+日志交付 |

## 参考资产（2026-08-12 大清理后）

- 11 个参考仓库克隆于 `reference/repos/`（SHA 钉死；目录未入版本控制），清单与可信分级：`references/REFERENCES.md`
- **A 级金标准**：owntone-server、pyatv、shairport-sync、Sunshine
- 已移除（不可信）：airplay-cli、cliairplay、airplay2-rs ×3、AirSend —— 明细 `archive/07-参考清理.md`
- 协议文档/网页落盘于 `references/`（未入版本控制）

## 协议发送链路（实现蓝本 = `docs/协议实现规范.md` 特性矩阵；本表为速览）

1. mDNS 发现（`_airplay._tcp`）/ 手动 IP → 2. 明文 GET /info（能力探测）→ 3. transient pair-setup（HKP 4，PIN 3939，SRP-6a/SHA-512/3072，M1-M4）→ 4. 加密 RTSP（ChaCha20-Poly1305 帧，IKM=64B K）→ 5. session SETUP（NTP timingPort）→ 6. 事件通道（反向 TCP，全程服务，连接重试）→ 7. **stream SETUP（type 96，ALAC，spf 352）→ 8. RECORD**（pyatv 序；owntone 序真机证伪至卡点，见规范 §6.3）→ 9. NTP 计时应答（先于流 SETUP 就绪）+ 1s sync → 10. ALAC RTP 加密发送（44100/s pacing，重传 backlog）→ 保活（2s 空 body /feedback + 事件服务）；断流→新连接重配。
