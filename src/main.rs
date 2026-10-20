#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod git;
mod sources;
mod ui;

use anyhow::{Context, Result};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        if std::env::args().len() == 1 {
            ui::error_box(&format!("{error:#}"));
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut home = exe.parent().context("找不到程序目录")?.to_path_buf();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|s| s == "--home") {
        home = PathBuf::from(args.get(i + 1).context("--home 缺少目录")?);
    }
    let command = args.first().map(String::as_str).unwrap_or("ui");
    if command == "ui" {
        return ui::run(home);
    }
    let mut app = engine::Engine::open(home)?;
    let value = match command {
        "inspect" | "status" => serde_json::to_value(app.inspect()?)?,
        "steam" => {
            app.switch("Steam")?;
            serde_json::to_value(app.inspect()?)?
        }
        "epic" => {
            app.switch("Epic")?;
            serde_json::to_value(app.inspect()?)?
        }
        "refresh" => {
            app.refresh(None)?;
            serde_json::to_value(app.inspect()?)?
        }
        "recover" => {
            app.recover()?;
            serde_json::to_value(app.inspect()?)?
        }
        "launch" => {
            app.launch()?;
            serde_json::to_value(app.inspect()?)?
        }
        "history" => serde_json::json!({"History":app.history()?}),
        "verify-steam" => serde_json::json!({"Mismatches":app.steam_mismatches()?}),
        "compact" => {
            app.compact()?;
            serde_json::json!({"ok":true})
        }
        _ => anyhow::bail!("未知命令：{command}"),
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(())
}
