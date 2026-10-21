# OrbShare

> 在同一局域网内，直接把电脑屏幕分享给浏览器和其他 OrbShare 客户端。

OrbShare 是一个开源的局域网屏幕共享与轻量会议工具。它不需要云端账号或外部会议服务器：发起端在本机完成屏幕采集、编码、信令和浏览器页面托管，观看者通过同一网络内的链接或二维码加入。

**English:** OrbShare is an open-source, privacy-first LAN screen sharing app. The host captures and serves the meeting locally, while guests join from a browser or another OrbShare client—without a cloud account or external meeting server.

## 主要功能

- 桌面或指定应用窗口共享，并可在会议中切换来源
- H.264 + WebRTC 低延迟传输，提供流畅、高清、文字优先三档画质
- 浏览器直接加入，支持手机、平板和电脑
- 局域网自动发现、手动 IP 加入、邀请链接和二维码
- 会议密码、成员列表、禁言、移除和举手
- 公屏聊天、常用表情和可开关弹幕
- 主持人激光笔、系统声音和麦克风传输
- 本地录屏、自动保存与快速定位文件
- 加入、离开和重新加入时间统计
- 中英文界面，默认跟随系统语言
- macOS 桌面/窗口采集与 VideoToolbox 硬件编码

## 隐私模型

OrbShare 的核心原则是“数据留在当前局域网”：

- 不要求注册账号
- 不连接 OrbShare 云端服务
- 屏幕、声音、聊天和控制消息由发起端与观看端直接传输
- 会议密码仅由发起端在本次会议中保存

请注意，任何能访问会议地址并通过密码校验的局域网设备都可能观看内容。敏感会议建议设置密码，并使用可信网络。

## 当前状态

项目仍处于活跃开发阶段。

| 平台 | 发起共享 | 原生加入 | 浏览器加入 |
|---|---:|---:|---:|
| macOS 12.3+ | 主要支持 | 支持 | 支持 |
| Windows 10/11 | 开发中 | 基础支持 | 支持 |
| iOS / iPadOS | — | 原型阶段 | 支持 |
| Android / Linux | 规划中 | 规划中 | 支持现代浏览器 |

当前采用发起端对观看者逐路发送的 P2P 架构，房间设置了 20 人安全上限。实际可承载人数取决于发起端 CPU/GPU、上行带宽、画质档位和画面变化程度；正式使用前请在目标网络中压测。

## 快速开始

### 环境要求

- Node.js 22+
- Rust stable
- macOS 构建需要 Xcode Command Line Tools
- Tauri 2 对应的系统构建依赖

### 开发运行

```bash
npm ci
npm run tauri dev
```

### 构建 macOS 应用

```bash
npm run build:mac:debug
```

产物默认位于：

```text
target/debug/bundle/macos/OrbShare.app
```

首次共享前，需要在“系统设置 → 隐私与安全性 → 屏幕与系统录音”中允许 OrbShare，然后完全退出并重新打开应用。

### 运行测试

```bash
npm run build
cargo test --workspace
```

macOS 完整验收基线：

```bash
./script/acceptance.sh
```

## 使用方式

1. 在发起端选择桌面或应用窗口，并选择画质。
2. 可选设置会议密码和共享电脑声音。
3. 开始共享后，复制邀请链接或展示二维码。
4. 观看者连接同一局域网，用浏览器打开链接即可加入。
5. 会议中可切换共享来源，无需重建房间。

## 项目结构

```text
src/                    React / TypeScript 桌面界面
src-tauri/              Tauri 主程序与会议编排
viewer/                 内置浏览器观看端
crates/orb-capture/     macOS / Windows 屏幕采集
crates/orb-encoder/     视频编码
crates/orb-media/       音视频路由与节流
crates/orb-webrtc/      WebRTC 会话
crates/orb-signaling/   局域网信令
crates/orb-discovery/   mDNS 发现
apps/ios/               iOS / iPadOS 加入端原型
docs/                   架构、验收与扩展说明
```

更详细的实现说明见 [局域网会议架构](docs/architecture/lan-meeting-architecture.md) 和 [验收矩阵](docs/acceptance/prd-v1-acceptance.md)。

## 路线图

- 完成 macOS 多设备长时间稳定性和 5/10/20 人压测
- 改善系统声音、麦克风和长时间录制的容错
- 完善 Windows 硬件采集与编码链路
- 推进 iOS/iPadOS、Android 和 Linux 原生加入端
- 评估可选的局域网转播节点，降低多人场景的发起端带宽压力

激光笔之外的复杂批注和观看端录制暂不在近期范围内。

## 参与贡献

欢迎提交 Issue 和 Pull Request。开始前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。安全问题请按 [SECURITY.md](SECURITY.md) 中的方式报告，不要直接公开可利用细节。

## 开源许可

本项目使用 [MIT License](LICENSE)。第三方依赖仍遵循各自许可证。
