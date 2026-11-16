//! 起動オプションの実行（V8）。解釈は [`crate::cli`]（GUI 非依存）。ここは
//!
//! - 起動の流れ（[`prepare`]）: 解釈 → ヘルプ・版・エラーの表示 → 標準入力 → 既存のウィンドウへの
//!   受け渡し → 自分で開く。外との接点（ダイアログ・標準入力・設定・受け渡し）は
//!   [`LaunchEnv`] で差し替えられる（自己診断・単体テストで止まらないように）。
//! - 既存のウィンドウの探索と `WM_COPYDATA` での受け渡し（送り側 [`forward_to_existing_window`]）。
//! - 受け取り側（[`App::receive_forward`]）: ファイルを今の開く処理（非同期）で開き、
//!   開いているファイルはそのタブへ切り替え、行・桁・読み取り専用・文字コードを適用する。
//!
//! # 受け取りの方針
//!
//! `WM_COPYDATA` の処理では重いことをしない。本体を復号して検査し、UI のメッセージ列に
//! 積んで `1`（受け取った）を返す。確認ダイアログなどで送り側を待たせないため。
//! 復号できない・版が違う・アプリが他の処理中（再入）のときは `0` を返し、送り側は
//! 自分で新しいウィンドウを開く。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowExW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    SendMessageTimeoutW, SetForegroundWindow, ShowWindow, MESSAGEBOX_STYLE, SMTO_ABORTIFHUNG,
    SW_RESTORE, WM_COPYDATA,
};

use crate::cli::{self, FileSpec, Forward, Info, Launch, WindowChoice};
use crate::core::{DocumentId, Limits, TaskContext, WorkerPool};
use crate::document::DocumentState;
use crate::file::{self, FileStamp, ReadOptions};
use crate::ui::editor::Language;
use crate::ui::statusbar::MessageKind;
use crate::ui::window::MAIN_CLASS;

use super::{App, DocView, RunOptions, UiMessage};

/// 既存のウィンドウの応答を待つ時間（ミリ秒）。
pub const FORWARD_TIMEOUT_MS: u32 = 5000;
/// 受け渡しを試すウィンドウの最大数（Z 順の手前から）。
const MAX_CANDIDATES: usize = 3;

// ------------------------------------------------------------------ 起動の流れ

/// 起動の流れが外に頼るもの。
pub trait LaunchEnv {
    /// 起動したプロセスのカレントフォルダー（相対パスの基準）。
    fn cwd(&self) -> PathBuf;
    /// ヘルプ・版・エラーを見せる（本番はメッセージボックス）。
    fn notice(&mut self, title: &str, text: &str, is_error: bool);
    /// 標準入力をすべて読む。
    fn read_stdin(&mut self) -> std::result::Result<Vec<u8>, String>;
    /// 設定 `open_in_existing_window`。
    fn open_in_existing_window(&mut self) -> bool;
    /// 既存のウィンドウへ渡す。受け取られたら `true`。
    fn forward(&mut self, files: &[FileSpec], stdin: Option<&[u8]>) -> bool;
}

/// [`prepare`] の結果。
pub enum Prepared {
    /// そのまま終了する（終了コード）。
    Exit(i32),
    /// 自己診断（`--self-test`）。
    SelfTest,
    /// 自分のウィンドウで起動する。
    Run(RunOptions),
}

/// 引数から、どう起動するかを決める。
pub fn prepare(args: &[OsString], env: &mut dyn LaunchEnv) -> Prepared {
    let launch = match cli::parse(args, &env.cwd()) {
        Ok(l) => l,
        Err(e) => {
            env.notice(
                "Linote",
                &format!("{}\n\n{}", e.message(), cli::usage()),
                true,
            );
            return Prepared::Exit(2);
        }
    };
    match launch.info {
        Some(Info::Help) => {
            env.notice("Linote の使い方", &cli::usage(), false);
            return Prepared::Exit(0);
        }
        Some(Info::Version) => {
            env.notice("Linote", &cli::version_text(), false);
            return Prepared::Exit(0);
        }
        None => {}
    }
    if launch.self_test {
        return Prepared::SelfTest;
    }
    let Launch {
        files,
        stdin: wants_stdin,
        window,
        mut no_session,
        adopt,
        pos,
        ..
    } = launch;
    let mut stdin = None;
    if wants_stdin {
        match env.read_stdin() {
            Ok(bytes) => stdin = Some(bytes),
            Err(e) => {
                env.notice("Linote", &format!("標準入力を読めませんでした: {e}"), true);
                // 開くものがなければ、前回のタブを開き直さず空のウィンドウにする。
                no_session = no_session || files.is_empty();
            }
        }
    }
    // 既存のウィンドウに渡す。標準入力が大きすぎるときは渡さず自分で開く。
    let forwardable = window != WindowChoice::New
        && adopt.is_none()
        && stdin
            .as_ref()
            .is_none_or(|b| b.len() <= cli::FORWARD_MAX_BYTES);
    let reuse = forwardable && (window == WindowChoice::Reuse || env.open_in_existing_window());
    if reuse && env.forward(&files, stdin.as_deref()) {
        return Prepared::Exit(0);
    }
    Prepared::Run(RunOptions {
        files,
        stdin,
        no_session,
        adopt,
        window_pos: pos,
        ..RunOptions::default()
    })
}

/// 本番の [`LaunchEnv`]。
pub struct SystemEnv;

impl LaunchEnv for SystemEnv {
    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_default()
    }

    fn notice(&mut self, title: &str, text: &str, is_error: bool) {
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
        };
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let style: MESSAGEBOX_STYLE = MB_OK
            | if is_error {
                MB_ICONERROR
            } else {
                MB_ICONINFORMATION
            };
        // SAFETY: 文字列は終端付きで、呼び出しの間有効。
        unsafe {
            MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style);
        }
    }

    fn read_stdin(&mut self) -> std::result::Result<Vec<u8>, String> {
        use std::io::{IsTerminal, Read};
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err("標準入力がパイプやリダイレクトになっていません".to_owned());
        }
        // 上限は巨大ファイルの扱いの上限と同じ程度にする（超えたら読み切らずに知らせる）。
        let cap = crate::bigfile::LargeModeThresholds::DEFAULT.max_bytes;
        let mut bytes = Vec::new();
        stdin
            .lock()
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > cap {
            return Err(format!("大きすぎます（{} MiB まで）", cap / (1024 * 1024)));
        }
        Ok(bytes)
    }

    fn open_in_existing_window(&mut self) -> bool {
        // 設定ファイルを読むだけ（壊れている・ないときは既定の真）。
        crate::appdata::AppPaths::from_env()
            .ok()
            .and_then(|p| std::fs::read_to_string(p.settings_file()).ok())
            .and_then(|text| crate::appdata::Settings::from_json(&text).ok())
            .is_none_or(|s| s.open_in_existing_window)
    }

    fn forward(&mut self, files: &[FileSpec], stdin: Option<&[u8]>) -> bool {
        forward_to_existing_window(files, stdin)
    }
}

// ------------------------------------------------------------------ 送り側

/// 既存の Linote のメインウィンドウ（表示中のもの）を、Z 順の手前から。`exclude_pid` の
/// プロセスのウィンドウは除く。
pub fn find_main_windows(exclude_pid: Option<u32>) -> Vec<HWND> {
    let mut found = Vec::new();
    let mut prev: Option<HWND> = None;
    // 無限に回らないよう、見る数に上限を置く。
    for _ in 0..64 {
        // SAFETY: 最上位ウィンドウの列挙（クラス名で絞る）。
        let Ok(hwnd) = (unsafe { FindWindowExW(None, prev, MAIN_CLASS, PCWSTR::null()) }) else {
            break;
        };
        prev = Some(hwnd);
        let mut pid = 0u32;
        // SAFETY: 取得したウィンドウのプロセス ID を読むだけ。
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        // SAFETY: 取得したウィンドウの状態を読むだけ。
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        if visible && Some(pid) != exclude_pid {
            found.push(hwnd);
        }
    }
    found
}

/// `hwnd` へ受け渡しの本体を `WM_COPYDATA` で送る。受け取られたら `true`。
pub fn send_forward(hwnd: HWND, data: &[u8]) -> bool {
    let mut pid = 0u32;
    // SAFETY: 相手のプロセス ID を読み、前面化を許す。
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
    }
    let cds = COPYDATASTRUCT {
        dwData: cli::COPYDATA_ID,
        cbData: data.len() as u32,
        lpData: data.as_ptr() as *mut _,
    };
    let mut result = 0usize;
    // SAFETY: `cds` と `data` は同期の送信が終わるまで有効。
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    sent.0 != 0 && result == 1
}

/// 起動要求を、開いている Linote のウィンドウ（Z 順の手前から数個）へ渡す。受け取られたら
/// `true`。見つからない・応答がない・時間切れ・拒否（別ユーザーや管理者権限のウィンドウ
/// など）は `false`（自分で新しいウィンドウを開く）。
pub fn forward_to_existing_window(files: &[FileSpec], stdin: Option<&[u8]>) -> bool {
    let candidates = find_main_windows(Some(std::process::id()));
    if candidates.is_empty() {
        return false;
    }
    let data = cli::encode_forward(files, stdin);
    if data.len() > cli::FORWARD_MAX_BYTES {
        return false;
    }
    candidates
        .into_iter()
        .take(MAX_CANDIDATES)
        .any(|hwnd| send_forward(hwnd, &data))
}

// ------------------------------------------------------------------ 受け取り側

/// `WM_COPYDATA` の処理（`dispatch` から）。起動要求なら UI のメッセージ列に積んで `1`、
/// そうでなければ `None`（既定の処理）。
pub(super) fn on_copy_data(app: &mut App, lparam: LPARAM) -> Option<LRESULT> {
    if lparam.0 == 0 {
        return None;
    }
    // SAFETY: WM_COPYDATA の lParam は COPYDATASTRUCT を指し、送信が終わるまで有効。
    let cds = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
    if cds.dwData != cli::COPYDATA_ID {
        return None;
    }
    let len = cds.cbData as usize;
    if len == 0 || len > cli::FORWARD_MAX_BYTES + 1024 * 1024 || cds.lpData.is_null() {
        return Some(LRESULT(0));
    }
    // SAFETY: 長さと先頭は送り側が保証している（検査済みの範囲）。
    let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, len) };
    let Some(forward) = cli::decode_forward(bytes) else {
        return Some(LRESULT(0));
    };
    app.sender().send(UiMessage::Run(Box::new(move |app| {
        app.receive_forward(forward)
    })));
    Some(LRESULT(1))
}

/// 標準入力の読み込みの結果。
type StdinLoad = crate::core::Result<(file::LoadedText, crate::ui::editor::DocLoader)>;

impl App {
    /// 既存のウィンドウに渡された起動要求を実行する。ファイルがなく標準入力もなければ、
    /// ウィンドウを前面に出すだけ。前回のタブの復元はしない。
    pub fn receive_forward(&mut self, forward: Forward) {
        for spec in &forward.files {
            self.open_spec(spec);
        }
        if let Some(bytes) = forward.stdin {
            self.open_stdin(bytes);
        }
        self.bring_to_front();
    }

    /// ウィンドウを前面に出す（最小化されていれば元に戻す）。
    pub(crate) fn bring_to_front(&self) {
        // SAFETY: 自分のウィンドウ。
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// 起動オプション 1 つ分のファイルを開く。開いているファイルはそのタブへ切り替え、
    /// 指定を適用する。読み込みは非同期で、行への移動・読み取り専用・文字コードは読み込みの
    /// 完了時に適用する（文書 ID は読み込み後に決まるので、パスで待たせる）。
    pub fn open_spec(&mut self, spec: &FileSpec) {
        let path = std::path::absolute(&spec.path).unwrap_or_else(|_| spec.path.clone());
        if path.is_dir() {
            self.set_status(
                &format!("フォルダーは開けません: {}", path.display()),
                MessageKind::Warning,
            );
            return;
        }
        let open = self
            .docs
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id());
        match open {
            Some(id) => self.apply_spec_to_open(id, &path, spec),
            None => {
                if let Some(line) = spec.line {
                    self.pending_jumps
                        .insert(path.clone(), (line, spec.column.unwrap_or(1)));
                }
                if spec.read_only {
                    self.pending_read_only.insert(path.clone());
                }
                if let Some(encoding) = spec.encoding {
                    self.pending_encodings.insert(path.clone(), encoding);
                }
                self.open_path(&path);
                // 開き始められなかった（読み込みが始まらなかった）ときに指定を残さない。
                if !self.opening.contains(&path) && !self.is_open_path(&path) {
                    self.forget_launch_state(&path);
                }
            }
        }
    }

    fn is_open_path(&self, path: &Path) -> bool {
        self.docs.iter().any(|d| d.state.path() == Some(path))
    }

    fn forget_launch_state(&mut self, path: &Path) {
        self.pending_jumps.remove(path);
        self.pending_read_only.remove(path);
        self.pending_encodings.remove(path);
    }

    /// すでに開いている文書へ、行・桁・読み取り専用・文字コードを適用する。
    fn apply_spec_to_open(&mut self, id: DocumentId, path: &Path, spec: &FileSpec) {
        let _ = self.activate(id);
        if spec.read_only {
            self.make_read_only(id);
        }
        let is_big = self.bigviews.contains_key(&id);
        if let (Some(encoding), false) = (spec.encoding, is_big) {
            // 開き直しの読み込みが終わってから移動する。確認は既存の開き直しと同じ
            // （未保存の変更があれば尋ねる）。
            if let Some(line) = spec.line {
                self.pending_jumps
                    .insert(path.to_path_buf(), (line, spec.column.unwrap_or(1)));
            }
            let (encoding, bom) = encoding;
            if let Err(e) = self.reopen_with_encoding(id, encoding, bom) {
                self.forget_launch_state(path);
                self.report_failure(&format!("{} を開き直せませんでした", path.display()), &e);
            }
            return;
        }
        if let Some(line) = spec.line {
            self.jump_to(id, line, spec.column.unwrap_or(1));
        }
    }

    /// 開いている文書を読み取り専用にする（巨大文書は元から読み取り専用）。
    pub(crate) fn make_read_only(&mut self, id: DocumentId) {
        if self.bigviews.contains_key(&id) {
            return;
        }
        let _ = self.update_state(id, |s| s.set_read_only(true));
        if matches!(
            self.index_of(id).map(|i| self.docs[i].view),
            Ok(DocView::Scintilla(_))
        ) {
            let _ = self.with_text(id, |ed| ed.set_read_only(true));
        }
    }

    // ---------------------------------------------------------------- 標準入力

    /// 標準入力の内容を、無題の文書として開く（文字コードは自動判定。変換と編集欄への
    /// 流し込みはワーカーで行う）。
    pub fn open_stdin(&mut self, bytes: Vec<u8>) {
        let loader = match self.create_loader(bytes.len() as u64) {
            Ok(l) => l,
            Err(e) => {
                self.report_failure("標準入力を開けませんでした", &e);
                return;
            }
        };
        let max = self.large_thresholds.max_bytes;
        let default_eol = self.settings.new_file.format.eol;
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext::detached().with_limits(Limits {
                max_items: None,
                max_bytes: Some(max),
            });
            let mut loader = loader;
            let stamp = FileStamp {
                len: bytes.len() as u64,
                modified: None,
                id: None,
            };
            let options = ReadOptions {
                encoding: None,
                default_eol,
            };
            let result: StdinLoad =
                file::decode_file_bytes(bytes, stamp, &options, &ctx).and_then(|mut loaded| {
                    let text = std::mem::take(&mut loaded.text);
                    loader.add(text.as_bytes())?;
                    Ok((loaded, loader))
                });
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_stdin(result)
            })));
        });
    }

    fn finish_stdin(&mut self, result: StdinLoad) {
        let (loaded, loader) = match result {
            Ok(r) => r,
            Err(e) => {
                self.report_failure("標準入力を開けませんでした", &e);
                return;
            }
        };
        let doc = match loader.finish() {
            Ok(d) => d,
            Err(e) => {
                self.report_failure("編集欄に読み込めませんでした", &e);
                return;
            }
        };
        let title = self.next_untitled_title();
        // 内容はどこにも保存されていないので、未保存の無題として扱う。
        let mut state = DocumentState::untitled();
        state.format = loaded.format;
        state.mark_unsaved();
        match self.add_loaded(state, doc, title, Language::PlainText, true) {
            Ok(_) if loaded.is_lossy() => self.set_status(
                "標準入力に読み取れないバイトがあり、「\u{fffd}」に置き換えて表示しています",
                MessageKind::Warning,
            ),
            Ok(_) => {}
            Err(e) => self.report_failure("標準入力を開けませんでした", &e),
        }
    }
}
