use crate::engine::{safe, unique};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub oid: Option<String>,
    pub hash: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub hash: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema: u32,
    pub mode: String,
    pub game_version: String,
    pub steam_build: String,
    #[serde(default)]
    pub steam_depot: String,
    pub official_package: String,
    pub anchors: Vec<Anchor>,
    pub files: Vec<Entry>,
}

#[derive(Clone)]
pub struct Store {
    pub dir: PathBuf,
    pub git: PathBuf,
}

pub struct BlobReader {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}
impl BlobReader {
    pub fn extract(&mut self, oid: &str, dest: &Path) -> Result<()> {
        check_oid(oid)?;
        writeln!(self.input, "{oid}")?;
        self.input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let fields: Vec<_> = header.split_whitespace().collect();
        ensure!(
            fields.len() == 3 && fields[0] == oid && fields[1] == "blob",
            "Git 文件对象缺失或类型错误"
        );
        let size: u64 = fields[2].parse()?;
        fs::create_dir_all(dest.parent().context("目标文件缺少父目录")?)?;
        let mut file = File::create(dest)?;
        let copied = std::io::copy(&mut self.output.by_ref().take(size), &mut file)?;
        ensure!(copied == size, "Git 文件对象已截断");
        let mut end = [0];
        self.output.read_exact(&mut end)?;
        ensure!(end == *b"\n", "Git 对象边界错误");
        file.sync_all()?;
        Ok(())
    }
}
impl Drop for BlobReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Store {
    pub fn reader(&self) -> Result<BlobReader> {
        let mut child = self
            .command()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Ok(BlobReader {
            child,
            input,
            output,
        })
    }
    pub fn open(home: &Path, git: &Path) -> Result<Self> {
        let dir = home.join("data/repository.git");
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
            let out = Command::new(git)
                .args(["init", "--bare"])
                .arg(&dir)
                .creation_flags(0x08000000)
                .output()?;
            ensure!(
                out.status.success(),
                "无法创建本地 Git 仓库：{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let store = Self {
            dir,
            git: git.to_path_buf(),
        };
        store.run(&["config", "core.autocrlf", "false"], None)?;
        store.run(&["config", "gc.auto", "0"], None)?;
        store.run(&["config", "core.logAllRefUpdates", "false"], None)?;
        Ok(store)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(&self.git);
        c.arg("--git-dir")
            .arg(&self.dir)
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_AUTHOR_NAME", "Endfield Switcher")
            .env("GIT_AUTHOR_EMAIL", "local@switcher.invalid")
            .env("GIT_COMMITTER_NAME", "Endfield Switcher")
            .env("GIT_COMMITTER_EMAIL", "local@switcher.invalid")
            .creation_flags(0x08000000);
        c
    }
    pub fn run(&self, args: &[&str], input: Option<&[u8]>) -> Result<String> {
        let mut c = self.command();
        c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        if input.is_some() {
            c.stdin(Stdio::piped());
        }
        let mut child = c.spawn().context("无法调用本机 Git")?;
        if let Some(bytes) = input {
            child.stdin.take().unwrap().write_all(bytes)?;
        }
        let out = child.wait_with_output()?;
        ensure!(
            out.status.success(),
            "Git 操作失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
    pub fn reference(&self, mode: &str) -> Result<Option<String>> {
        let name = branch(mode)?;
        let out = self
            .command()
            .args(["rev-parse", "--verify", name])
            .output()?;
        Ok(if out.status.success() {
            Some(String::from_utf8(out.stdout)?.trim().to_owned())
        } else {
            None
        })
    }
    pub fn snapshot(&self, mode: &str) -> Result<(String, Snapshot)> {
        let id = self.reference(mode)?.context("尚未创建平台快照")?;
        let snapshot = self.at(&id)?;
        ensure!(
            snapshot.mode == mode && snapshot.schema == 2,
            "Git 快照类型不匹配"
        );
        Ok((id, snapshot))
    }
    pub fn at(&self, commit: &str) -> Result<Snapshot> {
        check_oid(commit)?;
        let text = self.run(
            &["show", &format!("{commit}:.switcher-manifest.json")],
            None,
        )?;
        let s: Snapshot = serde_json::from_str(&text)?;
        ensure!(
            s.schema == 2 && matches!(s.mode.as_str(), "Steam" | "Epic"),
            "快照格式或平台无效"
        );
        let mut names = BTreeMap::new();
        for e in &s.files {
            safe(Path::new("C:\\switcher-path-check"), &e.path)?;
            ensure!(
                names.insert(e.path.to_lowercase(), ()).is_none(),
                "快照路径重复"
            );
            ensure!(e.oid.is_some() == e.hash.is_some(), "快照校验信息不完整");
            if let Some(oid) = &e.oid {
                check_oid(oid)?;
            }
            if let Some(hash) = &e.hash {
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                    "快照文件哈希格式错误"
                );
            }
        }
        Ok(s)
    }
    pub fn blob(&self, source: &Path) -> Result<String> {
        let out = self
            .command()
            .args(["hash-object", "-w", "--no-filters", "--"])
            .arg(source)
            .output()?;
        ensure!(
            out.status.success(),
            "保存 Git 文件对象失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
    #[cfg(test)]
    pub fn extract(&self, oid: &str, dest: &Path) -> Result<()> {
        check_oid(oid)?;
        fs::create_dir_all(dest.parent().context("文件缺少父目录")?)?;
        let mut child = self
            .command()
            .args(["cat-file", "blob", oid])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut file = File::create(dest)?;
        let copy = std::io::copy(&mut child.stdout.take().unwrap(), &mut file);
        file.sync_all()?;
        let out = child.wait_with_output()?;
        copy?;
        ensure!(out.status.success(), "Git 文件对象缺失或损坏：{oid}");
        Ok(())
    }
    pub fn commit(&self, snapshot: &Snapshot) -> Result<String> {
        let index = self.dir.join(format!("switcher-index-{}", unique()));
        let manifest = serde_json::to_vec(snapshot)?;
        let manifest_oid = self.run(&["hash-object", "-w", "--stdin"], Some(&manifest))?;
        let mut listing = format!("100644 {manifest_oid}\t.switcher-manifest.json\n");
        for e in &snapshot.files {
            if let Some(oid) = &e.oid {
                listing.push_str(&format!("100644 {oid}\t{}\n", e.path));
            }
        }
        let result = (|| -> Result<String> {
            let mut c = self.command();
            c.env("GIT_INDEX_FILE", &index)
                .args(["update-index", "--index-info"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut p = c.spawn()?;
            p.stdin.take().unwrap().write_all(listing.as_bytes())?;
            let output = p.wait_with_output()?;
            ensure!(
                output.status.success(),
                "建立快照目录失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let output = self
                .command()
                .env("GIT_INDEX_FILE", &index)
                .arg("write-tree")
                .output()?;
            ensure!(output.status.success(), "写入 Git 快照失败");
            let tree = String::from_utf8(output.stdout)?.trim().to_owned();
            let mut args = vec!["commit-tree", &tree];
            let parent = self.reference(&snapshot.mode)?;
            if let Some(ref parent) = parent
                && self.run(&["rev-parse", &format!("{parent}^{{tree}}")], None)? == tree
            {
                return Ok(parent.clone());
            }
            if let Some(ref parent) = parent {
                args.extend(["-p", parent]);
            }
            let msg = format!(
                "{} | game {} | Steam {} | official {}\n",
                snapshot.mode,
                snapshot.game_version,
                snapshot.steam_build,
                snapshot.official_package
            );
            self.run(&args, Some(msg.as_bytes()))
        })();
        let _ = fs::remove_file(&index);
        result
    }
    pub fn publish(&self, steam: &str, epic: &str) -> Result<()> {
        check_oid(steam)?;
        check_oid(epic)?;
        let old_steam = self.reference("Steam")?.unwrap_or_else(|| "0".repeat(40));
        let old_epic = self.reference("Epic")?.unwrap_or_else(|| "0".repeat(40));
        let input = format!(
            "start\nupdate refs/heads/steam {steam} {old_steam}\nupdate refs/heads/epic {epic} {old_epic}\nprepare\ncommit\n"
        );
        self.run(&["update-ref", "--stdin"], Some(input.as_bytes()))?;
        Ok(())
    }
    pub fn activate(&self, mode: &str) -> Result<()> {
        self.run(&["symbolic-ref", "HEAD", branch(mode)?], None)?;
        Ok(())
    }
    pub fn history(&self) -> Result<String> {
        self.run(
            &[
                "log",
                "--all",
                "-12",
                "--date=local",
                "--format=%h  %ad%n%s%n",
            ],
            None,
        )
    }
    pub fn compact(&self) -> Result<()> {
        self.run(
            &[
                "reflog",
                "expire",
                "--expire=now",
                "--expire-unreachable=now",
                "--all",
            ],
            None,
        )?;
        self.run(&["gc", "--quiet", "--prune=now"], None)?;
        Ok(())
    }
    /// Rebuild bounded chains with the same trees. Publish both heads atomically.
    pub fn retain_latest(&self, keep: usize) -> Result<BTreeMap<String, String>> {
        ensure!((1..=20).contains(&keep), "HistoryLimit 必须在 1 到 20 之间");
        let mut updates = BTreeMap::new();
        let mut heads = Vec::new();
        for mode in ["Steam", "Epic"] {
            let Some(head) = self.reference(mode)? else {
                return Ok(BTreeMap::new());
            };
            let commits = self.run(
                &[
                    "rev-list",
                    "--first-parent",
                    &format!("--max-count={}", keep + 1),
                    &head,
                ],
                None,
            )?;
            let ids: Vec<_> = commits.lines().collect();
            if ids.len() <= keep {
                heads.push(head);
                continue;
            }
            let mut parent: Option<String> = None;
            for id in ids[..keep].iter().rev() {
                let tree = self.run(&["rev-parse", &format!("{id}^{{tree}}")], None)?;
                let message = self.run(&["show", "-s", "--format=%B", id], None)?;
                let mut args = vec!["commit-tree", &tree];
                if let Some(ref p) = parent {
                    args.extend(["-p", p]);
                }
                parent = Some(self.run(&args, Some(message.as_bytes()))?);
            }
            let new = parent.context("历史裁剪未生成快照")?;
            updates.insert(head, new.clone());
            heads.push(new);
        }
        if !updates.is_empty() {
            self.publish(&heads[0], &heads[1])?;
        }
        Ok(updates)
    }
    pub fn objects_size(&self, entries: &[Entry]) -> Result<u64> {
        let ids: Vec<_> = entries.iter().filter_map(|e| e.oid.as_deref()).collect();
        for id in &ids {
            check_oid(id)?;
        }
        let mut total = 0u64;
        // Keep both pipe directions small to avoid stdin/stdout backpressure deadlocks.
        for chunk in ids.chunks(128) {
            let input = chunk.iter().map(|s| format!("{s}\n")).collect::<String>();
            let sizes = self.run(
                &["cat-file", "--batch-check=%(objecttype) %(objectsize)"],
                Some(input.as_bytes()),
            )?;
            for line in sizes.lines() {
                let size = line
                    .strip_prefix("blob ")
                    .context("快照对象缺失或类型错误")?
                    .parse::<u64>()?;
                total = total.checked_add(size).context("快照大小溢出")?;
            }
        }
        Ok(total)
    }
}
fn branch(mode: &str) -> Result<&'static str> {
    match mode {
        "Steam" => Ok("refs/heads/steam"),
        "Epic" => Ok("refs/heads/epic"),
        _ => bail!("未知平台"),
    }
}
fn check_oid(oid: &str) -> Result<()> {
    ensure!(
        oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
        "非法 Git 对象编号"
    );
    Ok(())
}
