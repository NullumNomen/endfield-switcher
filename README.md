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

游戏更新后，重新打开工具或点“重新适配更新”。相关程序仍运行时，自动适配会暂停；退出后手动重试。切换前会自动退出并恢复 Steam；若仍有未恢复事务，会保持 Steam 关闭，先恢复文件。

发生中断时保留恢复记录；界面提示恢复时点“恢复未完成的切换”。不要自行对游戏文件执行 `git reset --hard`。本地 `data/repository.git` 保存实际游戏差异文件，**不要上传到 GitHub**。共享目录不要通过任一启动器卸载或清空。

## 历史与磁盘空间

默认每个平台保留最近 **3 份**快照，相同内容不会重复创建提交；超出数量时以保留快照的原文件树重建有限历史，并回收旧对象。历史裁剪后提交编号会变化，不能把快照编号当作永久书签。

配置项 `HistoryLimit` 可设为 1–20；`MaxSnapshotBytes` 默认 8589934592（8 GiB），限制一次适配两平台差异总量及单次切换的暂存量。超过上限时停止处理，确认两平台资源同步或磁盘容量后再调整。该值不是整个游戏或仓库的硬磁盘配额；多个保留快照、Git 重打包与暂存会额外占空间。程序在适配、切换和 GC 前检查可用空间并保留 64 MiB 余量。

无未完成事务时，启动和适配完成会整理历史与缓存；普通成功切换也会清理暂存。存在恢复记录时禁止裁剪和清理，避免丢掉恢复所需对象。修复中断后可用 `compact` 立即整理。历史窗口显示保留上限和仓库磁盘占用。

## 从源码构建

安装 Rust、Git 和 Windows C/C++ 构建工具；推荐 Windows MSVC Rust 工具链及 Windows SDK。GNU LLVM 工具链还需要对应的资源编译器。

```powershell
cargo test --locked
cargo build --release --locked
```

成品为 `target/release/endfield-switcher.exe`，可重命名为 `EndfieldSwitcher.exe`。`build.rs` 将 `assets/endfield.ico` 嵌入 EXE。源码包含 Cargo.lock。

## GitHub Actions / 自动构建

工作流：`.github/workflows/windows.yml`。

- 提交到 `main`、向 `main` 提 PR：Windows x64 MSVC 环境执行格式检查、严格 Clippy、测试、Release 编译和打包。运行页面的 Artifacts 保留 14 天，可下载 EXE 和 ZIP。
- 推送 `v*` 标签：同样通过全部检查后，自动创建或更新该标签的**草稿 Release**。
- 手动：打开仓库 **Actions → Windows CI and draft release → Run workflow**，选择源码分支／标签并填写版本标签，例如 `v2.0.0-rc.1`。默认从所选源码构建，完成后在 Releases 中审核草稿。
- 草稿审核后在 GitHub 点 **Publish release**，下载文件才会公开出现在 Releases。工作流本身不自动发布。

每个草稿提供 `EndfieldSwitcher.exe`（直接下载）、包含图标／配置示例／文档的 ZIP、`SHA256SUMS.txt` 和 `BUILD_INFO.json`。来源信息记录实际源码提交、Rust 编译器和目标平台。发布包采用明确的文件白名单，不打包个人配置、游戏文件或 Git 游戏快照。

标签基础版本必须与 Cargo.toml 一致：当前 `2.0.0` 可用 `v2.0.0` 或 `v2.0.0-rc.1`。已存在的标签必须指向本次构建的提交，否则流程停止；不会强制改写标签。已发布的 Release 不允许重打包覆盖，请升级版本号并使用新标签。草稿重跑会替换构建产物和校验文件。

仅在网页上手动新建 Draft 不会启动该流程；请使用 Run workflow 或推送标签。构建无需额外 PAT，发布步骤使用本仓库临时 GITHUB_TOKEN 的 contents:write 权限，PR 只读。

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

本次发布通过 26 项自动测试，包括首次无旧版缓存初始化状态、Steam 恢复流程、Git 双分支事务、切换回滚、外部修改保护、路径约束与清单解析。现有安装此前完成过 Steam / Epic 双向切换及 Steam 游戏登录验证。首次联网初始化完整流程尚未在独立新机器上验证，因此以预览版发布。

## 项目与素材

非官方工具，与游戏发行商、Steam 和 Epic 无隶属关系。游戏内容及角色相关权利归原权利人所有。图标由 AI 根据本机官方游戏图标参考重绘，并非官方发布素材。仓库与 Release 不包含游戏程序、游戏资源、账号、令牌或用户的本地 Git 游戏快照。项目原创代码采用 [0BSD 许可证](LICENSE)，允许商用、修改和再分发，无署名或保留许可声明要求，并按许可证原文不提供担保。该授权不包含游戏角色等第三方素材的权利；第三方 Rust 依赖仍遵循各自许可证。
