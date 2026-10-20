use crate::{
    git::{Anchor, Entry, Snapshot, Store},
    sources::{self, Official, Steam},
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::windows::fs::MetadataExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::Diagnostics::ToolHelp::*,
    UI::Shell::ShellExecuteW,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Config {
    pub game_root: PathBuf,
    pub steam_manifest: PathBuf,
    #[serde(default = "steam_exe")]
    pub steam_exe: PathBuf,
    #[serde(default = "git_exe")]
    pub git_exe: PathBuf,
    pub steam_uri: String,
    pub epic_uri: String,
}
fn steam_exe() -> PathBuf {
    PathBuf::from(r"C:\Program Files (x86)\Steam\steam.exe")
}
fn git_exe() -> PathBuf {
    PathBuf::from(r"C:\Program Files\Git\cmd\git.exe")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Status {
    pub mode: String,
    pub game_version: String,
    pub steam_build: String,
    pub official_package: String,
    pub needs_refresh: bool,
    pub can_refresh: bool,
    pub block_reason: String,
    pub snapshot: String,
}
#[derive(Serialize, Deserialize)]
struct Active {
    mode: String,
    commit: String,
    stamp: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Journal {
    from: String,
    to: String,
    paths: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Refresh {
    return_mode: String,
    #[serde(default)]
    published: Option<String>,
    #[serde(default)]
    observed: Option<String>,
}

pub struct Engine {
    pub home: PathBuf,
    pub config: Config,
    pub store: Store,
    _lock: File,
}

static NEXT: AtomicU64 = AtomicU64::new(0);
pub fn unique() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
pub fn safe(root: &Path, relative: &str) -> Result<PathBuf> {
    ensure!(
        !relative.is_empty() && !relative.chars().any(|c| c.is_control() || c == ':'),
        "清单路径含非法字符"
    );
    let rel = relative.replace('\\', "/");
    ensure!(rel != ".switcher-manifest.json", "文件名与快照元信息冲突");
    let p = Path::new(&rel);
    ensure!(
        !p.is_absolute() && p.components().all(|c| matches!(c, Component::Normal(_))),
        "文件路径超出游戏目录：{relative}"
    );
    for part in rel.split('/') {
        ensure!(
            !part.ends_with(['.', ' ']) && !part.is_empty(),
            "文件路径不合法"
        );
    }
    let full = root.join(p);
    let mut current = Some(full.as_path());
    while let Some(path) = current {
        if let Ok(m) = fs::symlink_metadata(path) {
            ensure!(
                m.file_attributes() & 0x400 == 0,
                "受管路径包含目录联接或链接：{}",
                path.display()
            );
        }
        if path == root {
            break;
        }
        current = path.parent();
    }
    Ok(full)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("文件没有父目录")?)?;
    let tmp = path.with_extension(format!("tmp-{}", unique()));
    let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path).with_context(|| format!("保存记录失败：{}", path.display()))?;
    Ok(())
}
pub fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?;
    let b = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    Ok(serde_json::from_slice(b)?)
}
pub fn hash(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    ensure!(path.is_file(), "预期文件却发现目录：{}", path.display());
    let mut h = Sha256::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(Some(sources::hex(&h.finalize())))
}
pub fn game_version(root: &Path) -> String {
    let Ok(data) = fs::read(root.join("Endfield_Data/globalgamemanagers")) else {
        return "未知".into();
    };
    for pos in 4..data.len() {
        let len = u32::from_le_bytes(data[pos - 4..pos].try_into().unwrap()) as usize;
        if !(5..=30).contains(&len) || pos + len > data.len() {
            continue;
        }
        let b = &data[pos..pos + len];
        if !b.iter().all(|c| c.is_ascii_digit() || *c == b'.') {
            continue;
        }
        let Ok(text) = std::str::from_utf8(b) else {
            continue;
        };
        let parts: Vec<_> = text.split('.').collect();
        if parts.len() >= 3
            && parts.iter().all(|s| !s.is_empty())
            && parts[0].parse::<u32>().unwrap_or(1000) < 100
        {
            return text.into();
        }
    }
    "未知".into()
}
pub fn processes() -> Result<Vec<String>> {
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        ensure!(handle != INVALID_HANDLE_VALUE, "无法确认游戏进程状态");
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut result = Vec::new();
        let mut ok = Process32FirstW(handle, &mut entry);
        while ok != 0 {
            let n = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            result.push(String::from_utf16_lossy(&entry.szExeFile[..n]).to_lowercase());
            ok = Process32NextW(handle, &mut entry);
        }
        CloseHandle(handle);
        Ok(result)
    }
}
#[cfg(not(test))]
fn blocked() -> Result<()> {
    let p = processes()?;
    ensure!(
        !p.iter()
            .any(|n| n == "endfield.exe" || n == "platformprocess.exe"),
        "请先退出游戏，再切换或适配更新。"
    );
    ensure!(
        !p.iter().any(|n| n == "games.exe"),
        "请先退出终末地官方启动器（包括托盘），再切换或适配。"
    );
    ensure!(
        !p.iter().any(|n| n == "steam.exe"),
        "请先退出 Steam（包括托盘），再切换。需要校验时程序会自行启动 Steam。"
    );
    Ok(())
}
#[cfg(test)]
fn blocked() -> Result<()> {
    Ok(())
}
#[cfg(test)]
mod steam_restore_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn restores_previous_steam_after_success_and_failure() {
        for fail in [false, true] {
            let done = Cell::new(false);
            let reopened = Cell::new(false);
            let result = restore_steam_after(
                true,
                || {
                    done.set(true);
                    if fail {
                        bail!("切换失败")
                    }
                    Ok(())
                },
                || {
                    assert!(done.get());
                    reopened.set(true);
                    Ok(())
                },
            );
            assert!(reopened.get());
            assert_eq!(result.is_err(), fail);
        }
    }

    #[test]
    fn does_not_start_previously_closed_steam() {
        restore_steam_after(false, || Ok(()), || panic!("不应启动 Steam")).unwrap();
    }

    #[test]
    fn reports_restart_failure_without_losing_switch_error() {
        let result = restore_steam_after(true, || bail!("切换失败"), || bail!("启动失败"));
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("切换失败") && message.contains("启动失败"));
    }
}

fn restore_steam_after(
    reopen: bool,
    operation: impl FnOnce() -> Result<()>,
    restart: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let result = operation();
    if reopen {
        if let Err(error) = restart() {
            return match result {
                Ok(()) => Err(error),
                Err(original) => Err(original.context(format!("Steam 恢复失败：{error:#}"))),
            };
        }
    }
    result
}

pub fn shell(file: &str, args: &str) -> Result<()> {
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let verb = wide("open");
    let file = wide(file);
    let parameters = wide(args);
    unsafe {
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            if args.is_empty() {
                std::ptr::null()
            } else {
                parameters.as_ptr()
            },
            std::ptr::null(),
            1,
        );
        ensure!(
            (result as isize) > 32,
            "无法启动程序，请检查 Windows 的启动或权限提示"
        );
    }
    Ok(())
}

impl Engine {
    pub fn open(home: PathBuf) -> Result<Self> {
        fs::create_dir_all(home.join("data"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("data/rust-engine.lock"))?;
        lock.try_lock_exclusive()
            .context("切换器正在执行另一项操作，请稍后重试")?;
        let config_path = if home.join("config.json").exists() {
            home.join("config.json")
        } else {
            home.join("profiles.json")
        };
        let config: Config = load(&config_path)?;
        safe(&config.game_root, "Endfield.exe")?;
        let store = Store::open(&home, &config.git_exe)?;
        let app = Self {
            home,
            config,
            store,
            _lock: lock,
        };
        if app.store.reference("Steam")?.is_none() || app.store.reference("Epic")?.is_none() {
            if app.home.join("profiles.json").exists() {
                app.import_legacy()?;
            } else {
                ensure!(
                    app.store.reference("Steam")?.is_none()
                        && app.store.reference("Epic")?.is_none(),
                    "平台快照不完整，请保留 data 目录并恢复备份"
                );
            }
        }
        Ok(app)
    }
    pub fn progress(&self, message: &str) {
        let _ = atomic_json(
            &self.home.join("rust-progress.json"),
            &json!({"Message":message,"Time":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()}),
        );
    }
    fn import_legacy(&self) -> Result<()> {
        self.progress("正在把已有两套差异文件导入本地 Git 快照…");
        let old: Value = load(&self.home.join("profiles.json"))?;
        let cache = old["CacheRoot"].as_str().unwrap_or("data");
        let root = safe(&self.home, cache)?;
        let mut ids = Vec::new();
        for mode in ["Steam", "Epic"] {
            let mut files = Vec::new();
            for f in old["Files"].as_array().context("旧版差异清单缺失")? {
                let path = f["Path"].as_str().context("旧版文件路径缺失")?.to_owned();
                let expected = f[format!("{mode}Hash")].as_str().map(str::to_owned);
                let oid = if let Some(h) = &expected {
                    let p = safe(&root.join(mode), &path)?;
                    ensure!(hash(&p)?.as_ref() == Some(h), "旧版备份损坏：{mode}/{path}");
                    Some(self.store.blob(&p)?)
                } else {
                    None
                };
                files.push(Entry {
                    path,
                    oid,
                    hash: expected,
                });
            }
            let anchors = old["Anchors"]
                .as_array()
                .context("旧版版本标记缺失")?
                .iter()
                .map(|a| {
                    Ok(Anchor {
                        path: a["Path"].as_str().context("缺少标记路径")?.into(),
                        hash: a["Hash"].as_str().context("缺少标记哈希")?.into(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let snapshot = Snapshot {
                schema: 2,
                mode: mode.into(),
                game_version: game_version(&self.config.game_root),
                steam_build: old["SteamBuild"]
                    .as_str()
                    .context("旧版 Steam Build 缺失")?
                    .into(),
                steam_depot: String::new(),
                official_package: old["OfficialVersion"].as_str().unwrap_or("未知").into(),
                anchors,
                files,
            };
            ids.push(self.store.commit(&snapshot)?);
        }
        self.store.publish(&ids[0], &ids[1])?;
        self.progress("正在对 Git 历史进行二进制差异压缩…");
        self.store.compact()?;
        self.progress("Git 快照已建立。正在识别当前游戏文件…");
        Ok(())
    }
    fn actual(&self) -> Result<(String, String, Snapshot)> {
        let (sid, steam) = self.store.snapshot("Steam")?;
        let (eid, epic) = self.store.snapshot("Epic")?;
        let paths: BTreeSet<_> = steam
            .files
            .iter()
            .chain(&epic.files)
            .map(|f| f.path.clone())
            .collect();
        let mut values = BTreeMap::new();
        for p in paths {
            values.insert(p.clone(), hash(&safe(&self.config.game_root, &p)?)?);
        }
        let matches = |s: &Snapshot| s.files.iter().all(|f| values.get(&f.path) == Some(&f.hash));
        if matches(&steam) {
            return Ok(("Steam".into(), sid, steam));
        }
        if matches(&epic) {
            return Ok(("Epic".into(), eid, epic));
        }
        let source = sources::steam(&self.config)?;
        let is_steam = source
            .files
            .get("Endfield.exe")
            .map(|m| {
                sources::file_sha1(&self.config.game_root.join("Endfield.exe")).map(|h| h == m.sha1)
            })
            .transpose()?
            .unwrap_or(false);
        Ok((
            if is_steam {
                "SteamUpdated"
            } else {
                "EpicUpdated"
            }
            .into(),
            if is_steam { sid } else { eid },
            if is_steam { steam } else { epic },
        ))
    }
    fn stamp(&self) -> Result<String> {
        let root = self
            .config
            .game_root
            .join("Endfield_Data/StreamingAssets/VFS");
        let mut rows = Vec::new();
        fn walk(root: &Path, p: &Path, rows: &mut Vec<String>) -> Result<()> {
            if !p.exists() {
                return Ok(());
            }
            for e in fs::read_dir(p)? {
                let e = e?;
                let m = fs::symlink_metadata(e.path())?;
                ensure!(
                    m.file_attributes() & 0x400 == 0,
                    "游戏资源目录内出现链接，已停止版本识别"
                );
                if m.is_dir() {
                    walk(root, &e.path(), rows)?;
                } else if m.is_file() {
                    rows.push(format!(
                        "{}\t{}\t{}",
                        e.path().strip_prefix(root)?.display(),
                        m.len(),
                        m.last_write_time()
                    ));
                }
            }
            Ok(())
        }
        walk(&root, &root, &mut rows)?;
        rows.sort();
        let mut h = Sha256::new();
        for row in rows {
            h.update(row.as_bytes());
            h.update(b"\n");
        }
        Ok(sources::hex(&h.finalize()))
    }
    pub fn inspect(&self) -> Result<Status> {
        let block = blocked().err().map(|e| e.to_string()).unwrap_or_default();
        if self.store.reference("Steam")?.is_none() && self.store.reference("Epic")?.is_none() {
            let source = sources::steam(&self.config)?;
            ensure!(
                self.config.game_root.join("Endfield.exe").is_file(),
                "请先安装 Steam 版游戏，并正确设置 config.json 中的 GameRoot"
            );
            return Ok(Status {
                mode: "Uninitialized".into(),
                game_version: game_version(&self.config.game_root),
                steam_build: source.build,
                official_package: "待初始化".into(),
                needs_refresh: true,
                can_refresh: block.is_empty(),
                block_reason: block,
                snapshot: "尚未创建".into(),
            });
        }
        if self.home.join("switch.pending.json").exists() {
            return Ok(Status {
                mode: "Recovery".into(),
                game_version: game_version(&self.config.game_root),
                steam_build: "待恢复".into(),
                official_package: "待恢复".into(),
                needs_refresh: false,
                can_refresh: block.is_empty(),
                block_reason: block,
                snapshot: "".into(),
            });
        }
        let (mode, id, snapshot) = self.actual()?;
        let source = sources::steam(&self.config)?;
        let anchor_changed =
            snapshot
                .anchors
                .iter()
                .try_fold(false, |changed, a| -> Result<bool> {
                    Ok(changed
                        || hash(&safe(&self.config.game_root, &a.path)?)?.as_deref()
                            != Some(&a.hash))
                })?;
        let active_path = self.home.join("active.json");
        let stamp_changed = if active_path.exists() {
            let active: Active = load(&active_path)?;
            active.commit == id && self.stamp()? != active.stamp
        } else {
            false
        };
        let needs = mode.ends_with("Updated")
            || snapshot.steam_build != source.build
            || (!snapshot.steam_depot.is_empty() && snapshot.steam_depot != source.depot)
            || anchor_changed
            || stamp_changed
            || self.home.join("refresh.pending.json").exists();
        Ok(Status {
            mode,
            game_version: game_version(&self.config.game_root),
            steam_build: source.build,
            official_package: snapshot.official_package,
            needs_refresh: needs,
            can_refresh: block.is_empty(),
            block_reason: block,
            snapshot: id[..8].into(),
        })
    }
    fn save_active(&self, mode: &str, commit: &str) -> Result<()> {
        self.store.activate(mode)?;
        atomic_json(
            &self.home.join("active.json"),
            &Active {
                mode: mode.into(),
                commit: commit.into(),
                stamp: self.stamp()?,
            },
        )
    }
    pub fn switch(&mut self, mode: &str) -> Result<()> {
        ensure!(
            mode == "steam" || mode == "epic" || mode == "Steam" || mode == "Epic",
            "未知的平台"
        );
        #[cfg(not(test))]
        let reopen = {
            let p = processes()?;
            ensure!(
                !p.iter()
                    .any(|n| n == "endfield.exe" || n == "platformprocess.exe"),
                "请先退出游戏，再切换。"
            );
            ensure!(
                !p.iter().any(|n| n == "games.exe"),
                "请先退出终末地官方启动器（包括托盘），再切换。"
            );
            let running = p.iter().any(|n| n == "steam.exe");
            if running {
                shell(&self.config.steam_exe.to_string_lossy(), "-shutdown")?;
                let start = Instant::now();
                while processes()?.iter().any(|n| n == "steam.exe") {
                    ensure!(
                        start.elapsed() < Duration::from_secs(45),
                        "Steam 尚未退出，未开始切换。请处理 Steam 的退出提示后重试。"
                    );
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            running
        };
        #[cfg(test)]
        let reopen = false;
        let steam_exe = self.config.steam_exe.clone();
        restore_steam_after(
            reopen,
            || self.switch_inner(mode),
            || {
                shell(&steam_exe.to_string_lossy(), "")
                    .context("文件操作已结束，但重新打开 Steam 失败，请手动打开 Steam")
            },
        )
    }
    fn switch_inner(&mut self, mode: &str) -> Result<()> {
        blocked()?;
        if self.home.join("switch.pending.json").exists() {
            self.recover()?;
        }
        if self.inspect()?.needs_refresh {
            self.refresh(Some(mode))?;
            return Ok(());
        }
        self.checkout(mode)
    }
    fn checkout(&self, mode: &str) -> Result<()> {
        blocked()?;
        let (actual, from, previous) = self.actual()?;
        ensure!(
            actual == "Steam" || actual == "Epic",
            "当前文件未形成完整快照，请先适配更新"
        );
        self.checkout_from(mode, &from, &previous)
    }
    fn checkout_from(&self, mode: &str, from: &str, previous: &Snapshot) -> Result<()> {
        blocked()?;
        for e in &previous.files {
            ensure!(
                hash(&safe(&self.config.game_root, &e.path)?)? == e.hash,
                "切换前的文件已发生变化：{}",
                e.path
            );
        }
        let (to, target) = self.store.snapshot(mode)?;
        for a in &target.anchors {
            ensure!(
                hash(&safe(&self.config.game_root, &a.path)?)?.as_deref() == Some(&a.hash),
                "共享资源版本变化，请先适配更新"
            );
        }
        ensure!(
            sources::steam(&self.config)?.build == target.steam_build,
            "Steam 已更新，不能恢复旧快照"
        );
        if from == to {
            self.save_active(mode, &to)?;
            return Ok(());
        }
        self.progress(&format!("正在从 Git 准备 {} 差异文件…", mode));
        let paths: Vec<_> = previous
            .files
            .iter()
            .chain(&target.files)
            .map(|e| e.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // Stage beside the live files so replacement remains atomic even when the app is on C: and the game on D:.
        let stage = self.config.game_root.join(".endfield-switcher-stage/new");
        let mut blobs = self.store.reader()?;
        for e in &target.files {
            if let Some(oid) = &e.oid {
                let p = safe(&stage, &e.path)?;
                blobs.extract(oid, &p)?;
                ensure!(hash(&p)? == e.hash, "Git 目标文件校验失败：{}", e.path);
            }
        }
        let journal = Journal {
            from: from.into(),
            to: to.clone(),
            paths,
        };
        atomic_json(&self.home.join("switch.pending.json"), &journal)?;
        let result = (|| -> Result<()> {
            let entries: BTreeMap<_, _> =
                target.files.iter().map(|e| (e.path.as_str(), e)).collect();
            for name in &journal.paths {
                let live = safe(&self.config.game_root, name)?;
                if let Some(e) = entries.get(name.as_str()).filter(|e| e.oid.is_some()) {
                    fs::create_dir_all(live.parent().unwrap())?;
                    fs::rename(safe(&stage, &e.path)?, &live)
                        .with_context(|| format!("替换失败，文件可能正在使用：{name}"))?;
                } else if live.exists() {
                    let removed = safe(
                        &self
                            .config
                            .game_root
                            .join(".endfield-switcher-stage/removed"),
                        name,
                    )?;
                    fs::create_dir_all(removed.parent().unwrap())?;
                    fs::rename(&live, &removed)?;
                }
            }
            let (detected, commit, _) = self.actual()?;
            ensure!(detected == mode && commit == to, "切换后文件校验不通过");
            self.save_active(mode, &to)?;
            fs::remove_file(self.home.join("switch.pending.json"))?;
            Ok(())
        })();
        if let Err(error) = result {
            let restored = self.recover();
            return match restored {
                Ok(_) => Err(error.context("切换未完成，已恢复原快照")),
                Err(restore) => {
                    Err(error.context(format!("请保留恢复记录并退出相关程序后点恢复：{restore:#}")))
                }
            };
        }
        self.progress(&format!("已切换到 {mode}，快照 {}", &to[..8]));
        Ok(())
    }
    pub fn recover(&self) -> Result<()> {
        blocked()?;
        let path = self.home.join("switch.pending.json");
        if !path.exists() {
            return Ok(());
        }
        let journal: Journal = load(&path)?;
        let old = self.store.at(&journal.from)?;
        let new = self.store.at(&journal.to)?;
        ensure!(
            sources::steam(&self.config)?.build == old.steam_build,
            "游戏在中断后已更新，不能把旧快照强行恢复到新版。请保留恢复记录。"
        );
        for a in &old.anchors {
            ensure!(
                hash(&safe(&self.config.game_root, &a.path)?)?.as_deref() == Some(&a.hash),
                "共享资源在切换中断后发生变化，已停止回滚旧文件。"
            );
        }
        let old_map: BTreeMap<_, _> = old.files.iter().map(|e| (e.path.as_str(), e)).collect();
        let new_map: BTreeMap<_, _> = new.files.iter().map(|e| (e.path.as_str(), e)).collect();
        let mut blobs = self.store.reader()?;
        for name in &journal.paths {
            let current = hash(&safe(&self.config.game_root, name)?)?;
            let old = old_map.get(name.as_str()).and_then(|e| e.hash.as_ref());
            let new = new_map.get(name.as_str()).and_then(|e| e.hash.as_ref());
            ensure!(
                current.is_none() || current.as_ref() == old || current.as_ref() == new,
                "恢复时发现外部修改，请保留快照：{name}"
            );
        }
        for name in &journal.paths {
            let live = safe(&self.config.game_root, name)?;
            if let Some(e) = old_map.get(name.as_str()).filter(|e| e.oid.is_some()) {
                if hash(&live)? == e.hash {
                    continue;
                }
                let staged = safe(
                    &self
                        .config
                        .game_root
                        .join(".endfield-switcher-stage/recovery"),
                    name,
                )?;
                blobs.extract(e.oid.as_ref().unwrap(), &staged)?;
                ensure!(hash(&staged)? == e.hash, "恢复对象校验失败");
                fs::create_dir_all(live.parent().unwrap())?;
                fs::rename(staged, live)?;
            } else if live.exists() {
                fs::remove_file(live)?;
            }
        }
        for e in &old.files {
            ensure!(
                hash(&safe(&self.config.game_root, &e.path)?)? == e.hash,
                "恢复后的文件与原快照不符"
            );
        }
        self.save_active(&old.mode, &journal.from)?;
        fs::remove_file(path)?;
        Ok(())
    }
    pub fn history(&self) -> Result<String> {
        self.store.history()
    }
    pub fn compact(&self) -> Result<()> {
        self.progress("正在压缩 Git 对象并整理差异历史…");
        self.store.compact()
    }
    pub fn steam_mismatches(&self) -> Result<Vec<String>> {
        let source = sources::steam(&self.config)?;
        sources::mismatches(&self.config.game_root, &source, |n, total| {
            self.progress(&format!("正在校验 Steam 内容：{n}/{total}"))
        })
    }
    fn validate_steam(&self) -> Result<()> {
        blocked()?;
        let log = self
            .config
            .steam_exe
            .parent()
            .unwrap()
            .join("logs/content_log.txt");
        let offset = log.metadata().map(|m| m.len()).unwrap_or(0);
        shell(
            &self.config.steam_exe.to_string_lossy(),
            "steam://validate/4732690",
        )?;
        let deadline = Instant::now() + Duration::from_secs(2700);
        let mut scan_seen = false;
        let mut stable = 0;
        let result = (|| -> Result<()> {
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "Steam 校验等待超时。请让 Steam 完成更新，之后重新打开切换器即可续接。"
                );
                std::thread::sleep(Duration::from_secs(3));
                if let Ok(mut f) = File::open(&log) {
                    f.seek(SeekFrom::Start(offset.min(f.metadata()?.len())))?;
                    let mut text = String::new();
                    f.read_to_string(&mut text)?;
                    scan_seen |= text.contains("Validation: full scan in");
                }
                let source = match sources::steam(&self.config) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                self.progress(&format!(
                    "Steam 正在校验或更新：{:.1}/{:.1} MB",
                    source.downloaded as f64 / 1e6,
                    source.total as f64 / 1e6
                ));
                if scan_seen
                    && source.flags & 4 != 0
                    && source.flags & (256 | 512 | 1024 | 131072 | 262144) == 0
                    && source.downloaded >= source.total
                {
                    stable += 1;
                    if stable >= 2 {
                        let bad =
                            sources::mismatches(&self.config.game_root, &source, |n, total| {
                                self.progress(&format!("Steam 已完成，正在独立复核：{n}/{total}"))
                            })?;
                        ensure!(
                            bad.is_empty(),
                            "Steam 结束后仍有 {} 个文件不符（{}）。不会把不完整更新保存为快照。",
                            bad.len(),
                            bad.iter().take(3).cloned().collect::<Vec<_>>().join("、")
                        );
                        return Ok(());
                    }
                } else {
                    stable = 0;
                }
            }
        })();
        if !processes()?.iter().any(|n| n == "endfield.exe") {
            let _ = shell(&self.config.steam_exe.to_string_lossy(), "-shutdown");
            let end = Instant::now() + Duration::from_secs(30);
            while processes()?.iter().any(|n| n == "steam.exe") && Instant::now() < end {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        result
    }
    pub fn refresh(&mut self, requested: Option<&str>) -> Result<()> {
        blocked()?;
        ensure!(
            !self.home.join("switch.pending.json").exists(),
            "请先恢复上次未完成的切换"
        );
        let state = self.inspect()?;
        let path = self.home.join("refresh.pending.json");
        let mut journal: Refresh = if path.exists() {
            load(&path)?
        } else {
            Refresh {
                return_mode: if state.mode.starts_with("Epic") {
                    "Epic"
                } else {
                    "Steam"
                }
                .into(),
                published: None,
                observed: None,
            }
        };
        if let Some(mode) = requested {
            ensure!(mode == "Steam" || mode == "Epic", "无效平台");
            journal.return_mode = mode.into();
        }
        atomic_json(&path, &journal)?;
        if let Some(id) = &journal.published {
            if self.store.reference("Steam")?.as_ref() == Some(id) {
                let (actual, _, _) = self.actual()?;
                if actual == "Steam" || actual == "Epic" {
                    self.checkout(&journal.return_mode)?;
                } else {
                    let observed = if let Some(id) = &journal.observed {
                        id.clone()
                    } else {
                        ensure!(
                            self.steam_mismatches()?.is_empty(),
                            "已发布快照，但当前 Steam 内容还不完整，请先完成 Steam 校验"
                        );
                        let (_, base) = self.store.snapshot("Steam")?;
                        let id = self.observe(&base)?;
                        journal.observed = Some(id.clone());
                        atomic_json(&path, &journal)?;
                        id
                    };
                    self.checkout_from(
                        &journal.return_mode,
                        &observed,
                        &self.store.at(&observed)?,
                    )?;
                }
                fs::remove_file(path)?;
                self.store
                    .run(&["update-ref", "-d", "refs/switcher/observed"], None)?;
                return Ok(());
            }
        }
        self.progress("正在读取官方最新分发清单…");
        let mut official = Official::latest()?;
        if state.mode == "EpicUpdated" {
            let meta = &official.files["Endfield.exe"];
            let exe = self.config.game_root.join("Endfield.exe");
            ensure!(
                exe.is_file()
                    && exe.metadata()?.len() == meta.size
                    && sources::file_crc(&exe)? == meta.crc,
                "当前程序不匹配 Steam 清单或官方最新包，不能自动判断更新来源。游戏文件保持不变。"
            );
        }
        self.progress("正在核对 Steam 新版文件…");
        let bad = self.steam_mismatches()?;
        if !bad.is_empty() {
            self.validate_steam()?;
        }
        blocked()?;
        let steam = sources::steam(&self.config)?;
        let (sid, eid) = self.prepare(&steam, &mut official)?;
        let observed = self.observe(&self.store.at(&sid)?)?;
        journal.observed = Some(observed.clone());
        atomic_json(&path, &journal)?;
        self.store.publish(&sid, &eid)?;
        journal.published = Some(sid.clone());
        atomic_json(&path, &journal)?;
        self.checkout_from(&journal.return_mode, &observed, &self.store.at(&observed)?)?;
        fs::remove_file(path)?;
        self.store
            .run(&["update-ref", "-d", "refs/switcher/observed"], None)?;
        self.progress("更新适配完成，两平台快照已保存。");
        Ok(())
    }
    fn observe(&self, template: &Snapshot) -> Result<String> {
        let mut observed = template.clone();
        for e in &mut observed.files {
            let p = safe(&self.config.game_root, &e.path)?;
            e.hash = hash(&p)?;
            e.oid = if e.hash.is_some() {
                Some(self.store.blob(&p)?)
            } else {
                None
            };
        }
        let id = self.store.commit(&observed)?;
        self.store
            .run(&["update-ref", "refs/switcher/observed", &id], None)?;
        Ok(id)
    }
    fn prepare(&self, steam: &Steam, official: &mut Official) -> Result<(String, String)> {
        let old_epic = if self.store.reference("Epic")?.is_some() {
            Some(self.store.snapshot("Epic")?.1)
        } else {
            None
        };
        let old_map: BTreeMap<_, _> = old_epic
            .iter()
            .flat_map(|s| &s.files)
            .map(|e| (e.path.as_str(), e))
            .collect();
        let paths: BTreeSet<String> = steam
            .files
            .keys()
            .chain(official.files.keys())
            .cloned()
            .collect();
        let mut cases = BTreeSet::new();
        for p in &paths {
            ensure!(cases.insert(p.to_lowercase()), "两平台清单文件名大小写冲突");
        }
        let mut changed = Vec::new();
        for (i, name) in paths.iter().enumerate() {
            if i % 100 == 0 {
                self.progress(&format!("正在比较两平台资源：{i}/{}", paths.len()));
            }
            let live = safe(&self.config.game_root, name)?;
            let equal = match (steam.files.get(name), official.files.get(name)) {
                (Some(_), Some(e)) => {
                    live.is_file()
                        && live.metadata()?.len() == e.size
                        && sources::file_crc(&live)? == e.crc
                }
                _ => false,
            };
            if !equal {
                changed.push(name.clone());
            }
        }
        let stage = self.home.join("data/import-stage");
        let mut blobs = self.store.reader()?;
        let mut sfiles = Vec::new();
        let mut efiles = Vec::new();
        for (i, name) in changed.iter().enumerate() {
            self.progress(&format!("准备差异快照：{}/{}", i + 1, changed.len()));
            let live = safe(&self.config.game_root, name)?;
            if let Some(meta) = steam.files.get(name) {
                ensure!(
                    live.metadata()?.len() == meta.size
                        && (meta.size == 0 || sources::file_sha1(&live)? == meta.sha1),
                    "准备期间 Steam 文件被修改，请退出启动器后重试"
                );
                sfiles.push(Entry {
                    path: name.clone(),
                    oid: Some(self.store.blob(&live)?),
                    hash: hash(&live)?,
                });
            } else {
                sfiles.push(Entry {
                    path: name.clone(),
                    oid: None,
                    hash: None,
                });
            }
            if let Some(meta) = official.files.get(name).cloned() {
                let temp = safe(&stage, name)?;
                fs::create_dir_all(temp.parent().unwrap())?;
                let mut usable = false;
                if live.is_file()
                    && live.metadata()?.len() == meta.size
                    && sources::file_crc(&live)? == meta.crc
                {
                    fs::copy(&live, &temp)?;
                    usable = true;
                }
                if !usable {
                    if let Some(e) = old_map.get(name.as_str()).filter(|e| e.oid.is_some()) {
                        blobs.extract(e.oid.as_ref().unwrap(), &temp)?;
                        usable = temp.metadata()?.len() == meta.size
                            && sources::file_crc(&temp)? == meta.crc;
                    }
                }
                if !usable {
                    official.extract(name, &temp)?;
                }
                efiles.push(Entry {
                    path: name.clone(),
                    oid: Some(self.store.blob(&temp)?),
                    hash: hash(&temp)?,
                });
                fs::remove_file(temp)?;
            } else {
                efiles.push(Entry {
                    path: name.clone(),
                    oid: None,
                    hash: None,
                });
            }
        }
        let mut anchors = Vec::new();
        for name in ["Endfield_Data/globalgamemanagers", "Endfield_Data/app.info"] {
            if !changed.iter().any(|p| p == name) && safe(&self.config.game_root, name)?.is_file() {
                anchors.push(Anchor {
                    path: name.into(),
                    hash: hash(&safe(&self.config.game_root, name)?)?.unwrap(),
                });
            }
        }
        let mut epic_version = game_version(&self.config.game_root);
        if let Some(e) = efiles
            .iter()
            .find(|e| e.path == "Endfield_Data/globalgamemanagers")
            .filter(|e| e.oid.is_some())
        {
            let view = self.home.join("data/metadata-view");
            let p = safe(&view, &e.path)?;
            blobs.extract(e.oid.as_ref().unwrap(), &p)?;
            epic_version = game_version(&view);
            fs::remove_file(p)?;
        }
        let make = |mode: &str, files, version: String| Snapshot {
            schema: 2,
            mode: mode.into(),
            game_version: version,
            steam_build: steam.build.clone(),
            steam_depot: steam.depot.clone(),
            official_package: official.version.clone(),
            anchors: anchors.clone(),
            files,
        };
        let s = self
            .store
            .commit(&make("Steam", sfiles, game_version(&self.config.game_root)))?;
        let e = self.store.commit(&make("Epic", efiles, epic_version))?;
        atomic_json(&self.home.join("data/official-source.json"), &official.info)?;
        Ok((s, e))
    }
    pub fn launch(&mut self) -> Result<()> {
        if self.inspect()?.needs_refresh {
            self.refresh(None)?;
        }
        ensure!(
            !self.home.join("switch.pending.json").exists(),
            "请先恢复文件切换"
        );
        let (mode, _, _) = self.actual()?;
        match mode.as_str() {
            "Steam" => shell(&self.config.steam_uri, ""),
            "Epic" => shell(&self.config.epic_uri, ""),
            _ => bail!("请先完成版本适配"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_stay_inside_root() {
        let root = Path::new(r"C:\switcher-test");
        for p in [
            "../outside",
            "D:/other",
            "a\nmalicious",
            "a/../b",
            ".switcher-manifest.json",
            "a.",
        ] {
            assert!(safe(root, p).is_err(), "{p}");
        }
        assert_eq!(
            safe(root, "Endfield_Data/boot.config").unwrap(),
            root.join("Endfield_Data/boot.config")
        );
    }
    #[test]
    fn version_uses_unity_app_version() {
        let root = std::env::temp_dir().join(format!("switcher-{}", unique()));
        fs::create_dir_all(root.join("Endfield_Data")).unwrap();
        let mut data = Vec::from([5, 0, 0, 0]);
        data.extend(b"1.9.2");
        fs::write(root.join("Endfield_Data/globalgamemanagers"), data).unwrap();
        assert_eq!(game_version(&root), "1.9.2");
        fs::remove_file(root.join("Endfield_Data/globalgamemanagers")).unwrap();
        fs::remove_dir(root.join("Endfield_Data")).unwrap();
        fs::remove_dir(root).unwrap();
    }
    struct Fixture {
        root: PathBuf,
        app: Engine,
    }
    fn var(mut n: u64) -> Vec<u8> {
        let mut b = Vec::new();
        while n > 127 {
            b.push((n as u8 & 127) | 128);
            n >>= 7;
        }
        b.push(n as u8);
        b
    }
    fn bytes(n: u64, data: &[u8]) -> Vec<u8> {
        let mut b = var(n << 3 | 2);
        b.extend(var(data.len() as u64));
        b.extend(data);
        b
    }
    fn number(n: u64, v: u64) -> Vec<u8> {
        let mut b = var(n << 3);
        b.extend(var(v));
        b
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("endfield-rust-test-{}", unique()));
            fs::create_dir_all(root.join("game")).unwrap();
            fs::create_dir_all(root.join("depotcache")).unwrap();
            let mut files = Vec::new();
            let mut payload = Vec::new();
            for name in ["Endfield.exe", "nested/engine.dll", "steam_api64.dll"] {
                let s = safe(&root.join("data/Steam"), name).unwrap();
                fs::create_dir_all(s.parent().unwrap()).unwrap();
                fs::write(&s, format!("Steam {name}")).unwrap();
                let p = safe(&root.join("game"), name).unwrap();
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::copy(&s, &p).unwrap();
                let eh = if name != "steam_api64.dll" {
                    let e = safe(&root.join("data/Epic"), name).unwrap();
                    fs::create_dir_all(e.parent().unwrap()).unwrap();
                    fs::write(&e, format!("Epic {name}")).unwrap();
                    hash(&e).unwrap()
                } else {
                    None
                };
                files.push(json!({"Path":name,"SteamHash":hash(&s).unwrap(),"EpicHash":eh}));
                let sha = sources::file_sha1(&s).unwrap();
                let raw: Vec<u8> = (0..sha.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&sha[i..i + 2], 16).unwrap())
                    .collect();
                let mut mapping = bytes(1, name.as_bytes());
                mapping.extend(number(2, s.metadata().unwrap().len()));
                mapping.extend(bytes(5, &raw));
                payload.extend(bytes(1, &mapping));
            }
            fs::write(root.join("game/preferences.txt"), "leave me").unwrap();
            let mut metadata = number(2, 123);
            metadata.extend(number(4, 0));
            let mut manifest = Vec::new();
            for (magic, b) in [(0x71f617d0u32, payload), (0x1f4812be, metadata)] {
                manifest.extend(magic.to_le_bytes());
                manifest.extend((b.len() as u32).to_le_bytes());
                manifest.extend(b);
            }
            manifest.extend(0x32c415abu32.to_le_bytes());
            fs::write(root.join("depotcache/4732691_123.manifest"), manifest).unwrap();
            fs::write(root.join("app.acf"),"\"AppState\" { \"buildid\" \"1\" \"StateFlags\" \"4\" \"InstalledDepots\" { \"4732691\" { \"manifest\" \"123\" } } }").unwrap();
            let config = json!({"GameRoot":root.join("game"),"SteamManifest":root.join("app.acf"),"SteamExe":root.join("steam.exe"),"EpicLauncherRoot":root.join("official"),"SteamUri":"steam://test","EpicUri":"epic://test","SteamBuild":"1","OfficialVersion":"1.0","Files":files,"Anchors":[]});
            atomic_json(&root.join("profiles.json"), &config).unwrap();
            let app = Engine::open(root.clone()).unwrap();
            Self { root, app }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let base = std::env::temp_dir().canonicalize().unwrap();
            let root = self.root.canonicalize().unwrap();
            assert!(
                root.starts_with(&base)
                    && root
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("endfield-rust-test-")
            ); /* Leave small fixtures while the lock handle remains open; no recursive game-tree cleanup. */
        }
    }
    #[test]
    fn fresh_install_without_legacy_cache_reports_initialization() {
        let f = Fixture::new();
        let home = f.root.join("fresh");
        fs::create_dir_all(&home).unwrap();
        let config: Value = load(&f.root.join("profiles.json")).unwrap();
        atomic_json(&home.join("config.json"), &config).unwrap();
        let app = Engine::open(home).unwrap();
        let state = app.inspect().unwrap();
        assert_eq!(state.mode, "Uninitialized");
        assert!(state.needs_refresh && state.can_refresh);
        assert!(app.store.reference("Steam").unwrap().is_none());
    }

    #[test]
    fn git_branch_checkout_and_null_files() {
        let f = Fixture::new();
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
        f.app.checkout("Epic").unwrap();
        assert_eq!(f.app.inspect().unwrap().mode, "Epic");
        assert!(!f.root.join("game/steam_api64.dll").exists());
        f.app.checkout("Steam").unwrap();
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
        assert_eq!(
            fs::read_to_string(f.root.join("game/preferences.txt")).unwrap(),
            "leave me"
        );
        let tree = f
            .app
            .store
            .run(&["ls-tree", "-r", "--name-only", "refs/heads/steam"], None)
            .unwrap();
        assert!(!tree.contains("preferences"));
        assert!(tree.contains(".switcher-manifest.json"));
    }
    #[test]
    fn locked_file_rolls_back_entire_checkout() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.root.join("game/nested/engine.dll"))
            .unwrap();
        assert!(f.app.checkout("Epic").is_err());
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
        assert!(!f.root.join("switch.pending.json").exists());
        drop(lock);
    }
    #[test]
    fn bad_snapshot_hash_does_not_touch_live_files() {
        let f = Fixture::new();
        let (sid, _) = f.app.store.snapshot("Steam").unwrap();
        let (_, mut epic) = f.app.store.snapshot("Epic").unwrap();
        epic.files[0].hash = Some("0".repeat(64));
        let id = f.app.store.commit(&epic).unwrap();
        f.app.store.publish(&sid, &id).unwrap();
        assert!(f.app.checkout("Epic").is_err());
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
        assert!(!f.root.join("switch.pending.json").exists());
    }
    #[test]
    fn interrupted_checkout_restores_from_git() {
        let f = Fixture::new();
        let (s, steam) = f.app.store.snapshot("Steam").unwrap();
        let (e, epic) = f.app.store.snapshot("Epic").unwrap();
        let entry = &epic.files[0];
        let tmp = f.root.join("restore.tmp");
        f.app
            .store
            .extract(entry.oid.as_ref().unwrap(), &tmp)
            .unwrap();
        fs::rename(tmp, safe(&f.root.join("game"), &entry.path).unwrap()).unwrap();
        atomic_json(
            &f.root.join("switch.pending.json"),
            &Journal {
                from: s,
                to: e,
                paths: steam.files.iter().map(|e| e.path.clone()).collect(),
            },
        )
        .unwrap();
        assert_eq!(f.app.inspect().unwrap().mode, "Recovery");
        f.app.recover().unwrap();
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
    }
    #[test]
    fn pair_of_branch_refs_is_atomic() {
        let f = Fixture::new();
        let s = f.app.store.reference("Steam").unwrap().unwrap();
        let e = f.app.store.reference("Epic").unwrap().unwrap();
        assert!(f.app.store.publish(&s, &"1".repeat(40)).is_err());
        assert_eq!(f.app.store.reference("Steam").unwrap().unwrap(), s);
        assert_eq!(f.app.store.reference("Epic").unwrap().unwrap(), e);
    }
    #[test]
    fn external_platform_update_is_detected_and_not_overwritten() {
        let f = Fixture::new();
        fs::write(f.root.join("game/nested/engine.dll"), "new version").unwrap();
        assert!(f.app.inspect().unwrap().needs_refresh);
        assert!(f.app.checkout("Epic").is_err());
        assert_eq!(
            fs::read_to_string(f.root.join("game/nested/engine.dll")).unwrap(),
            "new version"
        );
    }
    #[test]
    fn refresh_normalizes_official_only_files_from_observed_snapshot() {
        let f = Fixture::new();
        let (_, mut steam) = f.app.store.snapshot("Steam").unwrap();
        let (_, mut epic) = f.app.store.snapshot("Epic").unwrap();
        let name = "new-official-config.bin";
        let p = f.root.join("game").join(name);
        fs::write(&p, "official configuration").unwrap();
        let oid = f.app.store.blob(&p).unwrap();
        steam.files.push(Entry {
            path: name.into(),
            oid: None,
            hash: None,
        });
        epic.files.push(Entry {
            path: name.into(),
            oid: Some(oid),
            hash: hash(&p).unwrap(),
        });
        let sid = f.app.store.commit(&steam).unwrap();
        let eid = f.app.store.commit(&epic).unwrap();
        let observed = f.app.observe(&steam).unwrap();
        f.app.store.publish(&sid, &eid).unwrap();
        assert_eq!(f.app.inspect().unwrap().mode, "SteamUpdated");
        f.app
            .checkout_from("Steam", &observed, &f.app.store.at(&observed).unwrap())
            .unwrap();
        assert!(!p.exists());
        assert_eq!(f.app.inspect().unwrap().mode, "Steam");
        f.app.checkout("Epic").unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "official configuration");
    }
}
