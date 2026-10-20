# 终末地切换器

<img src="assets/endfield.png" width="112" alt="终末地切换器图标">

Windows 上的《明日方舟：终末地》Steam / Epic（官方启动器）版本切换工具。共用一套游戏资源，以本地 Git 快照保存和切换平台差异文件。

**当前为 v2.0.0 预览版。** 主要验证环境是 Windows x64、Steam 国际服和 Epic 对应的官方客户端。首次适配需要网络、已安装的 Steam 游戏及本机 Steam 内容清单；不提供游戏本体或可分发的游戏快照。

## 功能

- 原生 Rust / Win32 界面，不依赖 Python、PowerShell 或 .NET 运行时。
- 显示当前平台、游戏版本、Steam Build、官方分发包版本与 Git 快照。
- 启动时检查版本，闲置时不定时扫描；切换和启动前重新核对。
- Steam 原本在运行时，切换前正常退出、操作结束后重新打开；不强制结束进程。
- 更新后比较两平台清单，只保存差异；Git 对相同内容去重。
- 同盘暂存、哈希校验、事务记录及中断恢复。

## 下载与首次设置

1. 从本仓库 Releases 下载 Windows x64 压缩包，解压到可写目录。
2. 安装 Git for Windows，确保 Steam 版游戏完整安装。至少先通过 Steam 正常启动一次游戏。
3. 将 `config.example.json` 复制为 `config.json`，按照自己的安装路径修改配置。
4. 退出游戏、Steam 和官方启动器后打开 `EndfieldSwitcher.exe`。首次运行会读取清单、核对 Steam 文件并建立两平台 Git 快照；必要时会调用 Steam 校验和下载官方差异文件。
5. 初始化完成后使用“切换到 Steam”或“切换到 Epic / 官方”，再点“启动当前版本”。

配置示例中的路径仅供参考，不会自动创建 Steam 安装记录或目录联接。`GameRoot` 必须是包含 `Endfield.exe` 的真实游戏目录，`SteamManifest` 是对应的 `appmanifest_4732690.acf`。`GitExe` 指向 Git for Windows 的 `mingw64/bin/git.exe`，`SteamExe` 指向本机 Steam。

`SteamUri`、`EpicUri` 是启动入口；Epic 入口可从本机 Epic 游戏快捷方式的 URL 获取。如果官方启动器目前指向另一份游戏目录，先使用启动器已有的定位游戏功能指向共享目录；本工具不自动改写启动器安装记录。已确认目录联接方案可用的现有用户可以继续沿用。

**首次初始化仅支持以 Steam 完整安装作为基线。** 不要用空目录或尚未下载完成的安装。两平台不同步时，差异大小可能显著增加。游戏版本和分发包版本是不同字段。

## 更新和恢复

保留 `config.json`、`active.json`、`data/` 和所有 `*.pending.json`，升级切换器时只替换 EXE 和图标。不要用发布压缩包覆盖自己的配置。

游戏更新后，重新打开工具或点“重新适配更新”。相关程序仍运行时，自动适配会暂停；退出后手动重试。切换前会自动退出并恢复 Steam。

发生中断时保留恢复记录；界面提示恢复时点“恢复未完成的切换”。不要自行对游戏文件执行 `git reset --hard`。本地 `data/repository.git` 保存实际游戏差异文件，**不要上传到 GitHub**。共享目录不要通过任一启动器卸载或清空。

## 从源码构建

安装 Rust、Git 和 Windows C/C++ 构建工具；推荐 Windows MSVC Rust 工具链及 Windows SDK。GNU LLVM 工具链还需要对应的资源编译器。

```powershell
cargo test --locked
cargo build --release --locked
```

成品为 `target/release/endfield-switcher.exe`，可重命名为 `EndfieldSwitcher.exe`。`build.rs` 将 `assets/endfield.ico` 嵌入 EXE。源码包含 Cargo.lock。

## 命令行

```powershell
.\EndfieldSwitcher.exe inspect
.\EndfieldSwitcher.exe steam
.\EndfieldSwitcher.exe epic
.\EndfieldSwitcher.exe refresh
.\EndfieldSwitcher.exe recover
.\EndfieldSwitcher.exe history
.\EndfieldSwitcher.exe compact
```

可追加 `--home <目录>` 指定配置和数据目录。Release EXE 使用 Windows GUI 子系统，脚本中需要读取输出时可用管道等待，例如 `& .\EndfieldSwitcher.exe inspect | Out-String`。

## 验证范围

本次发布通过 16 项自动测试，包括首次无旧版缓存初始化状态、Steam 恢复流程、Git 双分支事务、切换回滚、外部修改保护、路径约束与清单解析。现有安装此前完成过 Steam / Epic 双向切换及 Steam 游戏登录验证。首次联网初始化完整流程尚未在独立新机器上验证，因此以预览版发布。

## 项目与素材

非官方工具，与游戏发行商、Steam 和 Epic 无隶属关系。游戏内容及角色相关权利归原权利人所有。图标由 AI 根据本机官方游戏图标参考重绘，并非官方发布素材。仓库与 Release 不包含游戏程序、游戏资源、账号、令牌或用户的本地 Git 游戏快照。项目原创代码采用 [0BSD 许可证](LICENSE)，允许商用、修改和再分发，无署名或保留许可声明要求，并按许可证原文不提供担保。该授权不包含游戏角色等第三方素材的权利；第三方 Rust 依赖仍遵循各自许可证。
