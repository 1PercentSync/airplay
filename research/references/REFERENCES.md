# 参考资料清单（REFERENCES）

> 本文档登记所有被引用的仓库 / 文档 / 网站。选择依据：GitHub 检索（star 数 + 活跃度），原始检索结果 JSON 保存于本目录 `search_*.json`（已移出版本控制）。
> 仓库均浅克隆至 `reference/repos/`（去除 .git，SHA 见下表；该目录已移出版本控制，按 SHA 可随时精确重建）。
>
> **2026-08-12 大清理**：airplay-cli 被识别为 AI 生成（实测叙事不可信），同血统/同问题参考一并移除（见「三、已移除」）。

## 使用规范（查参考的操作细则）

> 硬性规则（用户指令，原文见 `AGENTS.md`）：**查参考时，只要某个在册参考项目存在对应特性的实现，就必须读，不得跳过。** 判定某参考“没有该点”时，须在规范矩阵对应格注明依据（查过什么、为什么没有、或可能在哪个部分）。

1. **先查规范**：先查 `docs/协议实现规范.md` 矩阵；格子标“未逐行取证”/缺失/未覆盖时才去读参考代码。
2. **全覆盖 + 分角色**：A 级定结论；B 级过路线适配关（AP1 行为不默认搬到 AP2，只当背景）；C 级只读代码字面，注释/散文不采信。
3. **读透**：能复述该参考在此点的完整行为（输入→构造→收发）才算读完；字段语义读写入方和读取方两侧。
4. **证据形式**：只认代码实际行为，引用到 `路径:行`；README/文档/注释的声称不算证据。
5. **双源**：协议字节级结论须两个独立 A 级（或 A 级+真机）一致；单源标 `[未验证]`，不进代码。
6. **分歧处理**：分歧是信息——先把各家行为+位置记录进规范矩阵，再按位序裁决：**真机（research/06）> A 级 > C 级（仅代码）> 协议文档**。
7. **回写**：新取证结论立即回填规范矩阵；代码注释用 `[evidence: 路径:行]`。
8. **红线**：禁引已移除项目（见三）；训练记忆一律 `[假设]` 起手再验证；禁 DeepWiki 类二手叙述；只读当前特性所需、读够就停；与 `docs/决策记录.md` 冲突时停下报告。
> 可信分级：**A 级**＝长期生产服役的人类项目（金标准）；**B 级**＝可信人类项目（代码可读但实战验证强度较低）；**C 级**＝人类项目但其散文声明未验证，仅代码可作参照。
> 协议结论一律以 A 级代码或真机实验为准。

## 一、现役参考（按可信分级）

### A 级（金标准）

| 仓库 | URL | Commit SHA | 定位 |
|---|---|---|---|
| owntone-server | github.com/owntone/owntone-server | cfcc1307e12990016484430935452ec7a41fdaba | C，AP1+AP2 发送端，量产服役（LMS/OwnTone）；协议行为第一仲裁 |
| pyatv | github.com/postlund/pyatv | b277a4c8222ecdcbaab8a24e3e713ca44765adb4 | Python，AP2 发送/控制库，真机覆盖最广 |
| shairport-sync | github.com/mikebrady/shairport-sync | 08af668a5d17b4714da38981dea4c9039263a4cc | 最成熟 AirPlay 1/2 接收端，接收端行为事实来源 |
| Sunshine | github.com/LizardByte/Sunshine | cf52f4b6f33cc9d143f0f6bb67c9891fe3563e54 | 用户指定的 Windows 音频捕获参考（WASAPI 链） |

### B 级（可信人类项目）

| 仓库 | URL | Commit SHA | 定位 |
|---|---|---|---|
| libraop | github.com/philippe44/libraop | 52e705106d3b4149c7f37ee643b69f96944e5786 | philippe44 RAOP v2 发送库（AirConnect/LMS 引擎），AP1 时代行为参照 |
| rust-raop-player | github.com/LinusU/rust-raop-player | c00676aeb44673e09dd3d25a4928409eb7df6322 | 上述的 Rust 移植，次要参照 |
| airplay2-receiver | github.com/openairplay/airplay2-receiver | 6c343d3679ddb561c61566985acaaf587d0a3bd3 | Python AP2 接收端（实验性，自述未完成），接收端细节旁证 |
| airplay-spec | github.com/openairplay/airplay-spec | 00063da0e7f16c20fbfb5d29331334da1a89590a | 社区逆向协议规范文本（AirPlay 豁免允许；仅作背景，不作结论依据） |
| AudioMirror | github.com/JannesP/AudioMirror | a9618d1ab4114e3e35e8e50ae804a4205315d1f2 | sysvad 派生 Windows 虚拟声卡（决策 D 备选评估用） |
| babelpod | github.com/afaden/babelpod | 12d06059c92d617847054916b31e3432899b8aa7 | 2018 年 JS 小项目（HomePod 直发），仅历史佐证 |

### C 级（人类项目，散文声明未验证）

| 仓库 | URL | Commit SHA | 定位 |
|---|---|---|---|
| airplay2-sender-cpp | github.com/akustikrausch/airplay2-sender-cpp | 8c4034263f1c265d25b3cfb88a090624760ad22a | Akustikrausch（FXChainPlayer 产品作者）C++ AP2 发送端；README 的"已验证"声明不予采信，**仅其代码字面行为可作消息格式参照**，且与 A 级冲突时以 A 级为准 |

## 二、保存的网页 / 文档（本目录，未入版本控制）

| 内容 | 文件 | 来源与可信度 |
|---|---|---|
| AirPlay 2 Internals（协议逆向文档，13 页） | `airplay2-internals/*.html` | emanuelecozzi.net（社区逆向文档，AirPlay 豁免允许；仅作背景） |
| pyatv 协议文档页 | `pyatv_protocols.html` | pyatv.dev（A 级项目官方文档） |
| MA airplay provider README | `ma_airplay_provider_README.md` | github.com/music-assistant/server —— **与 airplay-cli 同组织，声明不予采信** |
| Reddit: AirSend 发布帖 | `reddit_airsend.html` | reddit.com/r/rust（社区反馈；AirSend 本体已移除） |
| MS Learn: Loopback Recording | `ms_loopback_recording.md` | learn.microsoft.com（官方；WASAPI loopback 行为依据） |
| MS Learn: Volume Controls | `ms_volume_controls.md` | learn.microsoft.com（官方；端点音量回调依据） |
| RFC 5054 SRP (3072-bit group + Appendix B) | `rfc5054_srp.md` | rfc-editor.org（官方；HAP SRP N/g 与测试向量布局） |
| CPython 3.12 plistlib 整数宽度 | `cpython_3.12_plistlib_int.md` | github.com/python/cpython（pyatv 所用 bplist 库；仅整数编解码摘录） |

## 三、已移除（2026-08-12，不可信）

| 仓库 | SHA（溯源用） | 移除理由 |
|---|---|---|
| music-assistant/airplay-cli | bf57f69939628312d8b9a585502fffe80df7dcf2 | AI 生成特征明显（数千行 + "某日在某设备 A/B 验证"式不可证伪叙事）；其协议结论已逐条重锚 A 级或降级（见 research/archive/07） |
| music-assistant/cliairplay | 81a4413abf1254f1045f7cfa26c1543276598d3c | 同组织、airplay-cli 前身，同嫌疑 |
| Pabldi08/AirSend | ee55c8d5d2aa15959bd7e936a73fe9ff7d487758 | 决策 A 已判 AI 代码不可信（事件通道缺陷证伪） |
| Pabldi08/airplay2-rs | 1baeaae336ca3a9828e732500082f5fd1767d2fd | 同上（AirSend 实际依赖的协议栈） |
| lmcgartland/airplay2-rs | a7f019fe6246ebd9701201a8a5c31e9a15243956 | 决策 A 同判 |
| jburnhams/airplay2-rs | 527884f916d5860e39ab54b5cd272ba700f270bc | 决策 A 同判（克隆时已标注疑似 AI 生成） |

## 四、相关但未克隆

| 仓库 | 理由 |
|---|---|
| philippe44/AirConnect (4148★) | 桥接器，其发送引擎即 libraop，已克隆后者 |
| openairplay/goplay2 (420★) | Go AP2 接收端，如有需要再克隆 |
| hfujita/pulseaudio-raop2 (143★) | 2018 年停止维护，仅作历史参考 |
