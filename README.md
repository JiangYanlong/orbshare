# OrbShare

<p align="center">
  <strong>隐私优先的局域网屏幕共享与轻量会议工具</strong><br />
  不需要云端账号，不依赖外部会议服务器，浏览器打开链接即可观看。
</p>

<p align="center">
  简体中文 · <a href="README.en.md">English</a>
</p>

<p align="center">
  <a href="https://github.com/JiangYanlong/orbshare/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/JiangYanlong/orbshare/actions/workflows/ci.yml/badge.svg" /></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg" /></a>
  <img alt="Status: active development" src="https://img.shields.io/badge/status-active%20development-orange.svg" />
</p>

OrbShare 在发起端本机完成屏幕采集、硬件编码、信令和观看页面托管。观看者连接同一局域网后，可以通过邀请链接、二维码或 OrbShare 客户端加入。屏幕、声音、聊天和会议控制消息不需要经过 OrbShare 云端。

> [!IMPORTANT]
> 项目仍处于 `0.x` 活跃开发阶段。当前优先保证 macOS 发起共享和现代浏览器观看体验，不应将其视为已经完成企业级 SLA 验证的会议系统。

## 已实现功能

- 共享整个桌面或指定应用窗口，并可在会议中切换来源
- H.264 + WebRTC 低延迟传输，提供流畅、高清、文字优先三档画质
- 浏览器直接加入，适配手机、平板和电脑
- 局域网自动发现、手动 IP 加入、邀请链接和二维码
- 会议密码、成员列表、举手、禁言和移除成员
- 公屏聊天、常用表情和可开关弹幕
- 主持人激光笔、系统声音和麦克风传输
- 主持人本地录屏、自动保存和快速打开保存目录
- 加入、离开和重新加入时间统计
- 中英文界面，默认跟随系统语言
- macOS 桌面/窗口采集与 VideoToolbox 硬件编码

## 隐私与安全边界

OrbShare 的核心原则是“会议数据留在当前局域网”：

- 不要求注册 OrbShare 账号
- 不连接 OrbShare 云端服务
- 媒体和控制消息由发起端与观看端直接传输
- 会议密码仅由发起端在本次会议中保存
- 不应把内置 HTTP 观看地址直接暴露到公网

任何能够访问会议地址并通过密码校验的局域网设备都可能观看内容。敏感会议请设置密码、使用可信网络，并在结束后停止共享。

## 平台状态

| 平台 | 发起共享 | 原生加入 | 浏览器加入 |
|---|---:|---:|---:|
| macOS 12.3+ | 主要支持 | 支持 | 支持 |
| Windows 10/11 | 开发中 | 基础支持 | 支持 |
| iOS / iPadOS | — | 原型阶段 | 支持 |
| Android / Linux | 规划中 | 规划中 | 支持现代浏览器 |

当前采用发起端对观看者逐路发送的 P2P 架构，房间设置了 20 人安全上限。实际可承载人数取决于发起端 CPU/GPU、上行带宽、画质档位和画面变化程度；正式使用前请在目标网络中压测。

## macOS 签名与公证说明

当前开源仓库**不提供经过 Apple Developer ID 签名和 Apple 公证的官方下载包**。本地构建使用开发或临时签名；如果把未公证的 `.app`/`.dmg` 下载到另一台 Mac，Gatekeeper 可能显示“无法验证开发者”“应用已损坏”或阻止启动。

推荐的安全做法：

1. 普通体验者优先从源码在自己的 Mac 上构建。
2. 二次分发者使用自己的付费 Apple Developer 账号、`Developer ID Application` 证书和 App Store Connect 凭据完成签名及公证。
3. 公证成功后再分发 DMG，并在一台未参与开发的 Mac 上验证安装和屏幕录制权限。

Tauri 可以通过 `APPLE_SIGNING_IDENTITY` 及 Apple 公证凭据在构建时完成签名/公证。具体配置请遵循 [Tauri macOS 代码签名文档](https://v2.tauri.app/zh-cn/distribute/sign/macos/) 和 [Apple 公证文档](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)。请勿把证书、私钥、Apple ID 密码或 API Key 提交到仓库。

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

默认产物：

```text
target/debug/bundle/macos/OrbShare.app
target/debug/bundle/dmg/OrbShare_0.1.0_aarch64.dmg
```

首次共享前，需要在“系统设置 → 隐私与安全性 → 屏幕与系统录音”中允许 OrbShare，然后完全退出并重新打开应用。

### 运行测试

```bash
npm run build
cargo fmt --all --check
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

## Roadmap / 尚未完成

下面是计划，不代表当前版本已经支持：

### 稳定性与发布（优先）

- 完成 macOS 多设备长时间稳定性测试及 5/10/20 人压测
- 改善系统声音、麦克风和长时间录制的断线恢复与容错
- 建立可复现的 Developer ID 签名、公证和 GitHub Release 流程
- 增加崩溃诊断、网络质量指标和更完整的性能基线

### 多平台

- 完善 Windows 硬件采集、编码和正式安装包
- 完成 iOS/iPadOS 原生观看端
- 开发 Android 和 Linux 原生观看端
- 统一桌面端、移动端的会议状态与交互

### 多人扩展

- 评估可选的局域网轻量转播节点，降低多人场景的发起端带宽压力
- 增加自适应码率、拥塞控制和不同观看者的质量分层
- 完成更多路由器、访客网络和企业 Wi-Fi 环境兼容性测试

### 延后项目

- 复杂批注工具
- 观看端（Guest）本地录制
- 面向公网的云会议或中继服务

欢迎在 Issue 中讨论优先级，但请不要把 Roadmap 当作交付承诺。

## 参与贡献

欢迎提交 Issue 和 Pull Request。开始前请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。安全问题请使用仓库的 **Security → Report a vulnerability** 私密报告入口，不要公开可利用细节。

## 开源许可

本项目使用 [MIT License](LICENSE)。第三方依赖仍遵循各自许可证。
