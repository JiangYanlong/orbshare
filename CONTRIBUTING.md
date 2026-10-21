# Contributing to OrbShare

[简体中文](#简体中文) · [English](#english)

## 简体中文

感谢你愿意改进 OrbShare。

## 开始之前

1. 先搜索现有 Issue，避免重复工作。
2. 较大的功能请先开 Issue，说明使用场景、平台和验收方式。
3. 一次 Pull Request 尽量只解决一个问题。

## 本地验证

提交前至少运行：

```bash
npm ci
npm run build
cargo fmt --all --check
cargo test --workspace
```

涉及 macOS 采集、音频或打包时，还应在 macOS 上运行：

```bash
./script/acceptance.sh
```

## 代码约定

- 不要提交密钥、证书、真实会议密码、个人路径或录屏文件。
- 媒体发送路径优先保证“最新画面”，避免积压旧帧。
- 新增用户可见文字时，同时补充中英文翻译。
- 修复缺陷时尽量增加能复现问题的测试。
- 影响浏览器观看端时，同时检查桌面和移动布局。

## Pull Request 说明

请写清楚：

- 修改原因和用户影响
- 涉及的平台与模块
- 实际执行的测试命令
- 仍需人工验证的场景
- UI 变更前后的截图（如适用）

## English

Thank you for helping improve OrbShare.

### Before you start

1. Search existing Issues to avoid duplicate work.
2. Open an Issue before a large change and describe the use case, platform, and acceptance criteria.
3. Keep each pull request focused on one problem whenever possible.

### Local verification

Run at least:

```bash
npm ci
npm run build
cargo fmt --all --check
cargo test --workspace
```

For macOS capture, audio, or packaging changes, also run:

```bash
./script/acceptance.sh
```

### Project conventions

- Do not commit secrets, certificates, real meeting passwords, personal paths, or recordings.
- Prefer the newest video frame in media send paths; do not accumulate stale frames.
- Add both Chinese and English translations for user-visible text.
- Add a regression test when fixing a reproducible defect.
- Check both desktop and mobile layouts when changing the browser viewer.

### Pull request description

Include:

- Why the change is needed and its user impact
- Affected platforms and modules
- Commands actually used for verification
- Scenarios that still require manual validation
- Before/after screenshots for UI changes, when applicable
