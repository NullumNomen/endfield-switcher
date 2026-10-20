use crate::engine::{Engine, Status};
use anyhow::{Result, ensure};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Input::KeyboardAndMouse::EnableWindow, WindowsAndMessaging::*},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn error_box(text: &str) {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("终末地版本切换器").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

struct Outcome {
    status: Option<Status>,
    error: Option<String>,
    history: Option<String>,
    launched: bool,
}
struct App {
    home: PathBuf,
    labels: Vec<HWND>,
    buttons: Vec<HWND>,
    fonts: Vec<HFONT>,
    tx: mpsc::Sender<Outcome>,
    rx: mpsc::Receiver<Outcome>,
    busy: bool,
    auto_attempted: bool,
    command: String,
    started: f64,
}
impl App {
    unsafe fn create(&mut self, hwnd: HWND) {
        unsafe {
            let scale = GetDpiForSystem() as f64 / 96.0;
            let v = |n: i32| (n as f64 * scale) as i32;
            let font = CreateFontW(
                -v(16),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide("Microsoft YaHei UI").as_ptr(),
            );
            let title = CreateFontW(
                -v(25),
                0,
                0,
                0,
                700,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                DEFAULT_PITCH as u32,
                wide("Microsoft YaHei UI").as_ptr(),
            );
            self.fonts = vec![font, title];
            let instance = GetModuleHandleW(std::ptr::null());
            let control = |class: &str,
                           text: &str,
                           x: i32,
                           y: i32,
                           w: i32,
                           h: i32,
                           id: isize,
                           extra: u32,
                           f: HFONT| {
                let child = CreateWindowExW(
                    0,
                    wide(class).as_ptr(),
                    wide(text).as_ptr(),
                    WS_CHILD | WS_VISIBLE | extra,
                    v(x),
                    v(y),
                    v(w),
                    v(h),
                    hwnd,
                    id as HMENU,
                    instance,
                    std::ptr::null(),
                );
                SendMessageW(child, WM_SETFONT, f as usize, 1);
                child
            };
            self.labels.push(control(
                "STATIC",
                "终末地 · 版本切换器",
                24,
                22,
                530,
                42,
                0,
                0,
                title,
            ));
            self.labels.push(control(
                "STATIC",
                "正在检查版本与 Git 快照…",
                28,
                78,
                530,
                28,
                0,
                0,
                font,
            ));
            self.labels.push(control(
                "STATIC",
                "游戏版本：正在读取…",
                28,
                112,
                530,
                62,
                0,
                0,
                font,
            ));
            self.labels.push(control(
                "STATIC",
                "正在初始化 Rust 后端…",
                28,
                180,
                530,
                54,
                0,
                0,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "切换到 Steam",
                28,
                244,
                254,
                56,
                101,
                WS_TABSTOP,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "切换到 Epic / 官方",
                298,
                244,
                254,
                56,
                102,
                WS_TABSTOP,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "启动当前版本",
                28,
                312,
                524,
                38,
                103,
                WS_TABSTOP,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "重新适配更新",
                28,
                362,
                254,
                34,
                104,
                WS_TABSTOP,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "差异快照历史",
                298,
                362,
                254,
                34,
                106,
                WS_TABSTOP,
                font,
            ));
            self.buttons.push(control(
                "BUTTON",
                "恢复未完成的切换",
                28,
                404,
                524,
                32,
                105,
                WS_TABSTOP,
                font,
            ));
            ShowWindow(*self.buttons.last().unwrap(), SW_HIDE);
            self.labels.push(control(
                "STATIC",
                "切换前退出游戏和官方启动器。\nSteam 会自动退出，并在切换结束后重新打开。",
                28,
                446,
                524,
                52,
                0,
                0,
                font,
            ));
            SetTimer(hwnd, 1, 2000, None);
            self.job("inspect");
        }
    }
    unsafe fn text(&self, index: usize, text: &str) {
        unsafe {
            SetWindowTextW(self.labels[index], wide(text).as_ptr());
        }
    }
    unsafe fn job(&mut self, command: &str) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.command = command.into();
        self.started = now();
        unsafe {
            for b in &self.buttons {
                EnableWindow(*b, 0);
            }
            self.text(
                1,
                if command == "inspect" {
                    "正在识别游戏版本…"
                } else {
                    "正在处理，请稍候…"
                },
            );
        }
        let home = self.home.clone();
        let tx = self.tx.clone();
        let command = command.to_owned();
        std::thread::spawn(move || {
            let mut out = Outcome {
                status: None,
                error: None,
                history: None,
                launched: false,
            };
            match Engine::open(home) {
                Err(e) => out.error = Some(format!("{e:#}")),
                Ok(mut app) => {
                    let action = match command.as_str() {
                        "steam" => app.switch("Steam"),
                        "epic" => app.switch("Epic"),
                        "refresh" => app.refresh(None),
                        "recover" => app.recover(),
                        "launch" => app.launch().map(|_| out.launched = true),
                        "history" => app.history().map(|s| out.history = Some(s)),
                        _ => Ok(()),
                    };
                    if let Err(e) = action {
                        out.error = Some(format!("{e:#}"));
                    }
                    match app.inspect() {
                        Ok(s) => out.status = Some(s),
                        Err(e) => {
                            if out.error.is_none() {
                                out.error = Some(format!("{e:#}"));
                            }
                        }
                    }
                }
            }
            let _ = tx.send(out);
        });
    }
    unsafe fn tick(&mut self, hwnd: HWND) {
        unsafe {
            if self.busy {
                if let Ok(p) =
                    crate::engine::load::<serde_json::Value>(&self.home.join("rust-progress.json"))
                {
                    if p["Time"].as_f64().unwrap_or(0.0) >= self.started {
                        if let Some(text) = p["Message"].as_str() {
                            self.text(3, text);
                        }
                    }
                }
            }
            while let Ok(out) = self.rx.try_recv() {
                self.busy = false;
                let mut automatic = false;
                if let Some(s) = out.status {
                    let mode = match s.mode.as_str() {
                        "Steam" => "Steam 版本",
                        "Epic" => "Epic / 官方版本",
                        "SteamUpdated" => "Steam 已更新",
                        "EpicUpdated" => "官方文件已变化",
                        "Recovery" => "需要恢复上次切换",
                        _ => "待识别",
                    };
                    self.text(1, &format!("当前：{mode}    Git 快照：{}", s.snapshot));
                    self.text(
                        2,
                        &format!(
                            "游戏版本：{}\nSteam Build：{}    官方分发包：{}",
                            s.game_version, s.steam_build, s.official_package
                        ),
                    );
                    self.text(
                        3,
                        if s.needs_refresh {
                            if s.can_refresh {
                                "检测到更新，将自动重新生成平台差异快照。"
                            } else {
                                &s.block_reason
                            }
                        } else {
                            "当前版本已适配，可正常切换。"
                        },
                    );
                    for b in &self.buttons {
                        EnableWindow(*b, if s.mode == "Recovery" { 0 } else { 1 });
                    }
                    ShowWindow(
                        self.buttons[5],
                        if s.mode == "Recovery" {
                            SW_SHOW
                        } else {
                            SW_HIDE
                        },
                    );
                    EnableWindow(self.buttons[5], 1);
                    if !s.needs_refresh {
                        self.auto_attempted = false;
                    }
                    automatic = s.needs_refresh
                        && s.can_refresh
                        && !self.auto_attempted
                        && self.command == "inspect";
                }
                if let Some(e) = out.error {
                    self.text(3, &e);
                    automatic = false;
                    self.auto_attempted = true;
                    if self.command != "inspect" {
                        MessageBoxW(
                            hwnd,
                            wide(&e).as_ptr(),
                            wide("操作未完成").as_ptr(),
                            MB_OK | MB_ICONWARNING,
                        );
                    }
                }
                if let Some(h) = out.history {
                    MessageBoxW(
                        hwnd,
                        wide(&h).as_ptr(),
                        wide("本地 Git 差异快照历史").as_ptr(),
                        MB_OK | MB_ICONINFORMATION,
                    );
                }
                if out.launched {
                    ShowWindow(hwnd, SW_MINIMIZE);
                }
                if automatic {
                    self.auto_attempted = true;
                    self.job("refresh");
                }
            }
        }
    }
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let c = &*(l as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, c.lpCreateParams as isize);
        }
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if !p.is_null() {
            let app = &mut *p;
            match msg {
                WM_CREATE => {
                    app.create(hwnd);
                    return 0;
                }
                WM_TIMER => {
                    app.tick(hwnd);
                    return 0;
                }
                WM_COMMAND => {
                    let command = match w & 0xffff {
                        101 => "steam",
                        102 => "epic",
                        103 => "launch",
                        104 => "refresh",
                        105 => "recover",
                        106 => "history",
                        _ => "",
                    };
                    if !command.is_empty() {
                        app.job(command);
                    }
                    return 0;
                }
                WM_CLOSE => {
                    if app.busy {
                        MessageBoxW(
                            hwnd,
                            wide("正在处理快照，请等待操作完成后关闭。").as_ptr(),
                            wide("终末地版本切换器").as_ptr(),
                            MB_OK,
                        );
                    } else {
                        DestroyWindow(hwnd);
                    }
                    return 0;
                }
                WM_DESTROY => {
                    KillTimer(hwnd, 1);
                    for f in &app.fonts {
                        DeleteObject(*f);
                    }
                    PostQuitMessage(0);
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}
pub fn run(home: PathBuf) -> Result<()> {
    unsafe {
        SetProcessDPIAware();
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("EndfieldRustSwitcherV2");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hIcon: LoadIconW(instance, 1usize as *const u16),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..std::mem::zeroed()
        };
        ensure!(RegisterClassW(&wc) != 0, "无法注册切换器窗口");
        let (tx, rx) = mpsc::channel();
        let mut app = Box::new(App {
            home,
            labels: Vec::new(),
            buttons: Vec::new(),
            fonts: Vec::new(),
            tx,
            rx,
            busy: false,
            auto_attempted: false,
            command: String::new(),
            started: 0.0,
        });
        let scale = GetDpiForSystem() as f64 / 96.0;
        let width = (580.0 * scale) as i32;
        let height = (550.0 * scale) as i32;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            class.as_ptr(),
            wide("终末地切换器").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            (&mut *app as *mut App).cast(),
        );
        ensure!(!hwnd.is_null(), "创建切换器窗口失败");
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(())
}
