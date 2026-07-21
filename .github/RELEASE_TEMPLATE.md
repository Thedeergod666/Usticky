## 下载

| 平台 | 文件 |
|---|---|
| macOS (Apple Silicon) | `Usticky_aarch64.dmg` |
| macOS (Intel) | `Usticky_x64.dmg` |
| Windows | `Usticky_x64-setup.exe` |
| Linux | `Usticky_amd64.AppImage` / `Usticky_amd64.deb` |

## 安装说明（本版未签名）

- **macOS**：首次打开如遇"无法验证开发者"：右键 app → 打开；或终端执行 `xattr -cr /Applications/Usticky.app`。
- **Windows**：SmartScreen 提示时点"更多信息"→"仍要运行"。
- **Linux**：AppImage 先 `chmod +x` 直接运行；deb 用 `sudo dpkg -i` 安装。

## 使用

- 全局快捷键 `CmdOrCtrl+Shift+Space` 唤出快速添加
- 系统托盘左键切换浮窗显隐，右键菜单切 pin mode / 打开设置
- 数据纯本地 `todos.json`，不联网、不同步

详细变更见 [CHANGELOG](https://github.com/Thedeergod666/Usticky/blob/main/CHANGELOG.md)。
