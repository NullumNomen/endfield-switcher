use crate::engine::{Config, safe};
use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct SteamFile {
    pub size: u64,
    pub sha1: String,
}
#[derive(Clone, Debug)]
pub struct Steam {
    pub build: String,
    pub depot: String,
    pub flags: u64,
    pub downloaded: u64,
    pub total: u64,
    pub files: BTreeMap<String, SteamFile>,
}

pub fn steam(config: &Config) -> Result<Steam> {
    let text = fs::read_to_string(&config.steam_manifest).context("无法读取 Steam 安装记录")?;
    let app = parse_vdf(text.trim_start_matches('\u{feff}'))?;
    let a = app.get("AppState").context("Steam 安装记录不完整")?;
    let get = |key: &str| a.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let depot = a
        .pointer("/InstalledDepots/4732691/manifest")
        .and_then(Value::as_str)
        .context("请先让 Steam 完成游戏安装或更新")?
        .to_owned();
    ensure!(
        depot.bytes().all(|c| c.is_ascii_digit()),
        "Steam 清单编号非法"
    );
    let name = format!("4732691_{depot}.manifest");
    let candidates = [
        config
            .steam_exe
            .parent()
            .unwrap()
            .join("depotcache")
            .join(&name),
        config
            .steam_manifest
            .parent()
            .unwrap()
            .join("depotcache")
            .join(name),
    ];
    let path = candidates
        .iter()
        .find(|p| p.is_file())
        .context("本机缺少 Steam 内容清单，请先让 Steam 完成更新")?;
    let files = parse_depot(&fs::read(path)?, &depot)?;
    Ok(Steam {
        build: get("buildid"),
        depot,
        flags: get("StateFlags").parse().unwrap_or(0),
        downloaded: get("BytesDownloaded").parse().unwrap_or(0),
        total: get("BytesToDownload").parse().unwrap_or(0),
        files,
    })
}

fn parse_vdf(text: &str) -> Result<Value> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut tokens = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c.is_whitespace() {
            continue;
        }
        if c == '{' || c == '}' {
            tokens.push(c.to_string());
            continue;
        }
        ensure!(c == '"', "Steam 安装记录格式错误");
        let mut value = String::new();
        let mut closed = false;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if c == '"' {
                closed = true;
                break;
            }
            if c == '\\' && i < chars.len() {
                value.push(chars[i]);
                i += 1;
            } else {
                value.push(c);
            }
        }
        ensure!(closed, "Steam 正在改写安装记录，请稍后重试");
        tokens.push(value);
    }
    fn object(tokens: &[String], i: &mut usize, nested: bool) -> Result<Value> {
        let mut map = serde_json::Map::new();
        while *i < tokens.len() {
            if tokens[*i] == "}" {
                ensure!(nested, "Steam 记录多余括号");
                *i += 1;
                return Ok(Value::Object(map));
            }
            let key = tokens[*i].clone();
            *i += 1;
            let value = tokens.get(*i).context("Steam 安装记录已截断")?;
            *i += 1;
            let value = if value == "{" {
                object(tokens, i, true)?
            } else {
                Value::String(value.clone())
            };
            ensure!(map.insert(key, value).is_none(), "Steam 记录字段重复");
        }
        ensure!(!nested, "Steam 安装记录不完整");
        Ok(Value::Object(map))
    }
    object(&tokens, &mut 0, false)
}

enum Proto<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
fn varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *data.get(*pos).context("Steam 内容清单已截断")?;
        *pos += 1;
        ensure!(shift != 63 || b < 2, "Steam 清单整数溢出");
        value |= u64::from(b & 127) << shift;
        if b < 128 {
            return Ok(value);
        }
    }
    bail!("Steam 清单整数异常")
}
fn fields(data: &[u8]) -> Result<Vec<(u64, Proto<'_>)>> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < data.len() {
        let tag = varint(data, &mut pos)?;
        ensure!(tag >> 3 != 0, "Steam 清单字段错误");
        let value = match tag & 7 {
            0 => Proto::Number(varint(data, &mut pos)?),
            2 => {
                let len = usize::try_from(varint(data, &mut pos)?)?;
                let end = pos.checked_add(len).context("清单长度溢出")?;
                let b = data.get(pos..end).context("Steam 内容清单长度错误")?;
                pos = end;
                Proto::Bytes(b)
            }
            1 | 5 => {
                let len = if tag & 7 == 1 { 8 } else { 4 };
                let end = pos + len;
                let b = data.get(pos..end).context("Steam 清单已截断")?;
                pos = end;
                Proto::Bytes(b)
            }
            _ => bail!("不支持的 Steam 清单字段"),
        };
        out.push((tag >> 3, value));
    }
    Ok(out)
}
pub fn parse_depot(data: &[u8], gid: &str) -> Result<BTreeMap<String, SteamFile>> {
    let mut pos = 0;
    let mut payload = None;
    let mut meta = None;
    let mut ended = false;
    while pos + 4 <= data.len() {
        let magic = u32::from_le_bytes(data[pos..pos + 4].try_into()?);
        pos += 4;
        if magic == 0x32c415ab {
            ended = true;
            break;
        }
        let len = u32::from_le_bytes(
            data.get(pos..pos + 4)
                .context("Steam 清单已截断")?
                .try_into()?,
        ) as usize;
        pos += 4;
        let end = pos.checked_add(len).context("Steam 清单长度溢出")?;
        let section = data.get(pos..end).context("Steam 清单不完整")?;
        pos = end;
        match magic {
            0x71f617d0 => payload = Some(section),
            0x1f4812be => meta = Some(section),
            0x1b81b817 => {}
            _ => bail!("Steam 清单格式暂不支持"),
        }
    }
    ensure!(ended && pos == data.len(), "Steam 清单没有完整结尾");
    let metadata = fields(meta.context("Steam 清单缺少元信息")?)?;
    let mut got_gid = String::new();
    for (n, value) in metadata {
        match (n, value) {
            (2, Proto::Number(v)) => got_gid = v.to_string(),
            (4, Proto::Number(v)) => ensure!(v == 0, "Steam 清单文件名未解密，请先启动 Steam"),
            _ => {}
        }
    }
    ensure!(got_gid == gid, "Steam 清单编号不匹配");
    let mut out = BTreeMap::new();
    let mut names = HashMap::new();
    for (n, value) in fields(payload.context("Steam 清单缺少文件列表")?)? {
        if n != 1 {
            continue;
        }
        let Proto::Bytes(mapping) = value else {
            bail!("Steam 文件清单错误")
        };
        let mut name = String::new();
        let mut size = 0;
        let mut flags = 0;
        let mut hash = String::new();
        for (n, value) in fields(mapping)? {
            match (n, value) {
                (1, Proto::Bytes(b)) => name = std::str::from_utf8(b)?.replace('\\', "/"),
                (2, Proto::Number(v)) => size = v,
                (3, Proto::Number(v)) => flags = v,
                (5, Proto::Bytes(b)) => {
                    ensure!(b.len() == 20, "Steam 文件校验值错误");
                    hash = hex(b);
                }
                (7, Proto::Bytes(b)) => ensure!(b.is_empty(), "暂不支持 Steam 清单中的链接"),
                _ => {}
            }
        }
        if flags & 64 != 0 {
            continue;
        }
        safe(Path::new("C:\\switcher-path-check"), &name)?;
        ensure!(
            !hash.is_empty() && names.insert(name.to_lowercase(), ()).is_none(),
            "Steam 文件清单重复或缺少哈希"
        );
        out.insert(name, SteamFile { size, sha1: hash });
    }
    ensure!(!out.is_empty(), "Steam 文件清单为空");
    Ok(out)
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}
pub fn file_sha1(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha1::new();
    let mut buf = vec![0; 4 * 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}
pub fn file_crc(path: &Path) -> Result<u32> {
    let mut f = File::open(path)?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0; 4 * 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize())
}
pub fn mismatches(
    root: &Path,
    steam: &Steam,
    progress: impl Fn(usize, usize) + Sync,
) -> Result<Vec<String>> {
    let all: Vec<_> = steam.files.iter().collect();
    let completed = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for worker in 0..6 {
            let all = &all;
            let completed = &completed;
            let failures = &failures;
            let progress = &progress;
            scope.spawn(move || {
                for (name, entry) in all.iter().skip(worker).step_by(6) {
                    // Steam represents some empty files with an all-zero content hash.
                    let check = || -> Result<bool> {
                        let p = safe(root, name)?;
                        Ok(p.is_file()
                            && p.metadata()?.len() == entry.size
                            && (entry.size == 0 || file_sha1(&p)? == entry.sha1))
                    };
                    if !check().unwrap_or(false) {
                        failures.lock().unwrap().push((*name).clone());
                    }
                    let n = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 80 == 0 || n == all.len() {
                        progress(n, all.len());
                    }
                }
            });
        }
    });
    let mut out = failures.into_inner().unwrap();
    out.sort();
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct PackageEntry {
    pub index: usize,
    pub size: u64,
    pub crc: u32,
}
#[derive(Clone)]
struct Part {
    start: u64,
    end: u64,
    url: String,
}
pub struct HttpZip {
    client: Client,
    parts: Vec<Part>,
    total: u64,
    pos: u64,
    cache: BTreeMap<u64, Vec<u8>>,
}
impl HttpZip {
    fn open(info: &Value, client: Client) -> Result<Self> {
        let mut parts = Vec::new();
        let mut total = 0u64;
        for p in info
            .pointer("/pkg/packs")
            .and_then(Value::as_array)
            .context("官方没有返回游戏分发包")?
        {
            let url = p["url"].as_str().context("官方下载地址缺失")?;
            let parsed = reqwest::Url::parse(url)?;
            let host = parsed.host_str().unwrap_or("");
            ensure!(
                parsed.scheme() == "https"
                    && (host.ends_with(".hg-cdn.com") || host.ends_with(".gryphline.com")),
                "官方返回了尚未支持的下载域名"
            );
            let len: u64 = p["package_size"]
                .as_str()
                .context("官方分包大小缺失")?
                .parse()?;
            ensure!(len > 0, "官方分包大小无效");
            let end = total.checked_add(len).context("分包大小溢出")?;
            parts.push(Part {
                start: total,
                end,
                url: url.to_owned(),
            });
            total = end;
        }
        ensure!(!parts.is_empty(), "官方分发包为空");
        Ok(Self {
            client,
            parts,
            total,
            pos: 0,
            cache: BTreeMap::new(),
        })
    }
    fn fetch(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let mut pos = start;
        let mut out = Vec::with_capacity((end - start) as usize);
        while pos < end {
            let part = self
                .parts
                .iter()
                .find(|p| pos >= p.start && pos < p.end)
                .context("读取超出官方分包范围")?;
            let until = end.min(part.end);
            let lo = pos - part.start;
            let hi = until - part.start - 1;
            let response = self
                .client
                .get(&part.url)
                .header("Range", format!("bytes={lo}-{hi}"))
                .header("Accept-Encoding", "identity")
                .timeout(Duration::from_secs(90))
                .send()?;
            ensure!(
                response.status() == 206,
                "官方 CDN 未正确支持分段下载 ({})",
                response.status()
            );
            let expected = format!("bytes {lo}-{hi}/");
            ensure!(
                response
                    .headers()
                    .get("Content-Range")
                    .and_then(|s| s.to_str().ok())
                    .unwrap_or("")
                    .starts_with(&expected),
                "下载分段位置不匹配"
            );
            let mut b = Vec::new();
            response.take(until - pos + 1).read_to_end(&mut b)?;
            ensure!(b.len() as u64 == until - pos, "下载分段长度不匹配");
            out.extend_from_slice(&b);
            pos = until;
        }
        Ok(out)
    }
}
impl Read for HttpZip {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total || buf.is_empty() {
            return Ok(0);
        }
        const BLOCK: u64 = 4 * 1024 * 1024;
        let key = self.pos / BLOCK * BLOCK;
        if !self.cache.contains_key(&key) {
            let mut error = None;
            for attempt in 0..3 {
                match self.fetch(key, (key + BLOCK).min(self.total)) {
                    Ok(b) => {
                        if self.cache.len() >= 4 {
                            self.cache.pop_first();
                        }
                        self.cache.insert(key, b);
                        error = None;
                        break;
                    }
                    Err(e) => {
                        error = Some(e);
                        std::thread::sleep(Duration::from_secs(attempt + 1));
                    }
                }
            }
            if let Some(e) = error {
                return Err(io::Error::other(format!("{e:#}")));
            }
        }
        let b = &self.cache[&key];
        let off = (self.pos - key) as usize;
        let n = buf.len().min(b.len() - off);
        buf[..n].copy_from_slice(&b[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}
impl Seek for HttpZip {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let p = match from {
            SeekFrom::Start(v) => v as i128,
            SeekFrom::Current(v) => self.pos as i128 + v as i128,
            SeekFrom::End(v) => self.total as i128 + v as i128,
        };
        if p < 0 || p > self.total as i128 {
            return Err(io::Error::other("ZIP 读取位置越界"));
        }
        self.pos = p as u64;
        Ok(self.pos)
    }
}
pub struct Official {
    pub version: String,
    pub info: Value,
    pub zip: zip::ZipArchive<HttpZip>,
    pub files: BTreeMap<String, PackageEntry>,
}
impl Official {
    pub fn latest() -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let request = json!({"seq":"1","proxy_reqs":[{"kind":"get_latest_game","get_latest_game_req":{"version":"","appcode":"YDUTE5gscDZ229CW","channel":"6","sub_channel":"9999","device_id":"endfield-rust-switcher"}}]});
        let response: Value = client
            .post("https://launcher.gryphline.com/api/proxy/batch_proxy")
            .json(&request)
            .send()?
            .error_for_status()?
            .json()?;
        let info = response
            .pointer("/proxy_rsps/0/get_latest_game_rsp")
            .context("官方接口响应格式不受支持")?
            .clone();
        let version = info["version"]
            .as_str()
            .context("官方分发版本缺失")?
            .to_owned();
        let mut zip = zip::ZipArchive::new(HttpZip::open(&info, client)?)?;
        let mut files = BTreeMap::new();
        let mut names = HashMap::new();
        for index in 0..zip.len() {
            let file = zip.by_index(index)?;
            if file.is_dir() {
                continue;
            }
            let name = file.name().replace('\\', "/");
            safe(Path::new("C:\\switcher-path-check"), &name)?;
            ensure!(
                names.insert(name.to_lowercase(), ()).is_none(),
                "官方 ZIP 文件名重复"
            );
            files.insert(
                name,
                PackageEntry {
                    index,
                    size: file.size(),
                    crc: file.crc32(),
                },
            );
        }
        ensure!(
            files.contains_key("Endfield.exe") && files.contains_key("GameAssembly.dll"),
            "官方包缺少游戏程序"
        );
        Ok(Self {
            version,
            info,
            zip,
            files,
        })
    }
    pub fn extract(&mut self, name: &str, target: &Path) -> Result<()> {
        let meta = self
            .files
            .get(name)
            .context("官方包中缺少目标文件")?
            .clone();
        fs::create_dir_all(target.parent().unwrap())?;
        let mut from = self.zip.by_index(meta.index)?;
        let mut to = File::create(target)?;
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let n = from.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut to, &buffer[..n])?;
        }
        to.sync_all()?;
        ensure!(
            target.metadata()?.len() == meta.size && file_crc(target)? == meta.crc,
            "官方差异文件校验失败：{name}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vdf_nested_and_truncated() {
        let v = parse_vdf("\"AppState\" { \"buildid\" \"1\" \"InstalledDepots\" { \"4732691\" { \"manifest\" \"123\" } } }").unwrap();
        assert_eq!(
            v["AppState"]["InstalledDepots"]["4732691"]["manifest"],
            "123"
        );
        assert!(parse_vdf("\"AppState\" { \"x\"").is_err());
    }
    #[test]
    fn varint_rejects_truncation_and_overflow() {
        assert!(varint(&[128], &mut 0).is_err());
        assert!(varint(&[255; 11], &mut 0).is_err());
        assert_eq!(varint(&[172, 2], &mut 0).unwrap(), 300);
    }
    #[test]
    fn steam_zero_length_file_with_zero_hash_is_valid() {
        let root =
            std::env::temp_dir().join(format!("endfield-empty-test-{}", crate::engine::unique()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("space.txt"), []).unwrap();
        let s = Steam {
            build: "1".into(),
            depot: "1".into(),
            flags: 4,
            downloaded: 0,
            total: 0,
            files: BTreeMap::from([(
                "space.txt".into(),
                SteamFile {
                    size: 0,
                    sha1: "0".repeat(40),
                },
            )]),
        };
        assert!(mismatches(&root, &s, |_, _| {}).unwrap().is_empty());
        fs::remove_file(root.join("space.txt")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
