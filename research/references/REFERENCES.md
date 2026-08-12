# 参考资料清单（REFERENCES）

> 本文档登记所有被引用的仓库 / 文档 / 网站。选择依据：GitHub 检索（star 数 + 活跃度），原始检索结果 JSON 保存于本目录 `search_*.json`。
> 仓库均已浅克隆至 `reference/repos/`（去除 .git，SHA 见下表）。

## 一、发送端实现（核心参考）

| 仓库 | URL | Commit SHA | Stars | 最后推送 | 选用理由 |
|---|---|---|---|---|---|
| AirSend | github.com/Pabldi08/AirSend | ee55c8d5d2aa15959bd7e936a73fe9ff7d487758 | 29 | 2026-06-25 | Rust+Tauri，Windows→HomePod AirPlay 2 发送端，目标与本项目完全重合（README 声称，待代码验证） |
| owntone-server | github.com/owntone/owntone-server | cfcc1307e12990016484430935452ec7a41fdaba | 2538 | 2026-08-06 | C，声称支持 AirPlay 1+2 发送，长期活跃 |
| pyatv | github.com/postlund/pyatv | b277a4c8222ecdcbaab8a24e3e713ca44765adb4 | 1159 | 2026-08-10 | Python，Apple TV/AirPlay 设备客户端库，含 RAOP/AirPlay 推流 |
| libraop | github.com/philippe44/libraop | 52e705106d3b4149c7f37ee643b69f96944e5786 | 121 | 2026-08-04 | C，RAOP 发送端库（AirConnect 引擎），活跃 |
| airplay2-sender-cpp | github.com/akustikrausch/airplay2-sender-cpp | 8c4034263f1c265d25b3cfb88a090624760ad22a | 57 | 2026-06-23 | C++，声称"已验证"的 AirPlay 2 ALAC 发送端 |
| airplay2-rs (lmcgartland) | github.com/lmcgartland/airplay2-rs | a7f019fe6246ebd9701201a8a5c31e9a15243956 | 96 | 2026-02-09 | Rust，AirPlay 2 探索 |
| airplay2-rs (jburnhams) | github.com/jburnhams/airplay2-rs | 527884f916d5860e39ab54b5cd272ba700f270bc | 4 | 2026-05-04 | Rust AirPlay 2 库，较新（含大量文档，疑似 AI 辅助生成，需谨慎） |
| airplay2-rs (Pabldi08 fork) | github.com/Pabldi08/airplay2-rs | 1baeaae336ca3a9828e732500082f5fd1767d2fd | — | — | **AirSend 实际依赖的协议栈**（Cargo.lock 钉死此 SHA），含 airplay-client/pairing/rtsp/audio/timing 等 10 个 crate |
| rust-raop-player | github.com/LinusU/rust-raop-player | c00676aeb44673e09dd3d25a4928409eb7df6322 | 32 | 2025-11-01 | Rust，RAOP v2 发送（带同步） |
| **airplay-cli** | github.com/music-assistant/airplay-cli | bf57f69939628312d8b9a585502fffe80df7dcf2 | — | 2026-08 | **第四独立实现**：统一 RAOP+AP2 native 发送端 CLI（owntone/libraop 血统），Music Assistant 生产环境日用；DESIGN.md 为最佳实测协议文档 |
| cliairplay | github.com/music-assistant/cliairplay | 81a4413abf1254f1045f7cfa26c1543276598d3c | 3 | 2026-07-29 | 已废弃（被 airplay-cli 取代），留存溯源 |
| babelpod | github.com/afaden/babelpod | 12d06059c92d617847054916b31e3432899b8aa7 | 236 | 2023-11-13 | 向 HomePod 直接发送音频的小项目 |

## 二、协议理解（接收端 / 规范）

| 仓库 | URL | Commit SHA | Stars | 最后推送 | 选用理由 |
|---|---|---|---|---|---|
| shairport-sync | github.com/mikebrady/shairport-sync | 08af668a5d17b4714da38981dea4c9039263a4cc | 8806 | 2026-08-09 | 最成熟 AirPlay 1/2 接收端，协议行为事实来源 |
| airplay2-receiver | github.com/openairplay/airplay2-receiver | 6c343d3679ddb561c61566985acaaf587d0a3bd3 | 2343 | 2026-06-29 | Python AirPlay 2 接收端 |
| airplay-spec | github.com/openairplay/airplay-spec | 00063da0e7f16c20fbfb5d29331334da1a89590a | 84 | — | 非官方 AirPlay 协议规范文本（社区来源，适用豁免） |

## 三、音频捕获（Windows）

| 仓库 | URL | Commit SHA | Stars | 最后推送 | 选用理由 |
|---|---|---|---|---|---|
| Sunshine | github.com/LizardByte/Sunshine | cf52f4b6f33cc9d143f0f6bb67c9891fe3563e54 | 40101 | 2026-08-12 | 用户指定的捕获参考实现 |
| AudioMirror | github.com/JannesP/AudioMirror | a9618d1ab4114e3e35e8e50ae804a4205315d1f2 | 250 | 2022-10-15 | 开源 Windows 虚拟音频驱动候选 |

## 四、保存的网页 / 文档

| 内容 | 文件 | 来源 |
|---|---|---|
| AirPlay 2 Internals（协议逆向文档，13 页） | `airplay2-internals/*.html` | emanuelecozzi.net/docs/airplay2 |
| Reddit: AirSend 发布帖（含真实用户反馈） | `reddit_airsend.html` | reddit.com/r/rust |
| pyatv 协议文档页 | `pyatv_protocols.html` | pyatv.dev/documentation/protocols |
| MA airplay provider README | `ma_airplay_provider_README.md` | github.com/music-assistant/server |

## 五、相关但未克隆

| 仓库 | 理由 |
|---|---|
| philippe44/AirConnect (4148★) | 桥接器，其发送引擎即 libraop，已克隆后者 |
| openairplay/goplay2 (420★) | Go AP2 接收端，如有需要再克隆 |
| hfujita/pulseaudio-raop2 (143★) | 2018 年停止维护，仅作历史参考 |
