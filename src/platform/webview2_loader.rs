//! WebView2 の自前ローダー。`WebView2Loader.dll` も `webview2-com-sys` のローダー関数も使わない
//! （単体 EXE 要件。`docs/architecture.md` の「WebView2」）。
//!
//! 手順:
//! 1. レジストリからインストール済みランタイムの場所と版を探す。EdgeUpdate の Clients キー
//!    `{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` の値 `pv`（版）と `location`（`EBWebView` があれば
//!    それも）を、HKLM の `SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients`、
//!    HKLM の `SOFTWARE\Microsoft\EdgeUpdate\Clients`、HKCU の同じキーの順に調べる。
//! 2. `<runtime>\EBWebView\x64\EmbeddedBrowserWebView.dll` を `LoadLibraryExW` で読み込む。
//!    候補の組み立てと版の検査は純粋な関数（[`dll_candidates`]、[`is_usable_version`]）で、
//!    実在の確認だけが入出力。
//! 3. その DLL の `CreateWebViewEnvironmentWithOptionsInternal` を `GetProcAddress` で取り、呼ぶ。
//!
//! ランタイムが見つからなければ [`crate::core::Error::Unavailable`] を返す。
//! **プレビューを初めて開くまで、このモジュールの関数を呼ばない**（レジストリの確認も
//! DLL の読み込みも、ここから始まる）。
//!
//! 使ってよいのは `webview2-com` の COM インターフェース型とハンドラーの補助だけ。
//! `CreateCoreWebView2EnvironmentWithOptions` など、`WebView2Loader.dll` を要求する関数は
//! 呼ばない（`scripts/check-imports.sh` と `objdump -p` で確認する）。
//!
//! 担当: タスク H。

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use webview2_com::CreateCoreWebView2EnvironmentCompletedHandler;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler, ICoreWebView2Environment,
};
use windows::core::{s, w, Interface, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

use crate::core::{Error, Result};
use crate::platform::system::{os_to_wide, to_wide};

/// Evergreen ランタイムの EdgeUpdate 上の製品 ID。
pub const RUNTIME_CLIENT_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// ランタイムの本体 DLL の、ランタイムフォルダーからの相対パス。
pub const EMBEDDED_DLL_RELATIVE: &str = r"EBWebView\x64\EmbeddedBrowserWebView.dll";

/// ユーザーデータフォルダーの `%LOCALAPPDATA%` からの相対パス。
pub const USER_DATA_RELATIVE: &str = r"Linote\WebView2";

/// レジストリのハイブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    LocalMachine,
    CurrentUser,
}

/// 探索順（ハイブとキーのパス）。`architecture.md` §10 のとおり。
pub const REGISTRY_SOURCES: [(Hive, &str); 4] = [
    (
        Hive::LocalMachine,
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients",
    ),
    (Hive::LocalMachine, r"SOFTWARE\Microsoft\EdgeUpdate\Clients"),
    (
        Hive::CurrentUser,
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients",
    ),
    (Hive::CurrentUser, r"SOFTWARE\Microsoft\EdgeUpdate\Clients"),
];

/// Clients キーから読んだ値（無い値は `None`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryEntry {
    /// `pv`: 版（`120.0.2210.91` のような数の並び）。
    pub pv: Option<String>,
    /// `location`: ランタイムの場所（版のフォルダー自身、またはその親の `Application`）。
    pub location: Option<String>,
    /// `EBWebView`: `EBWebView` フォルダー自身（新しい版にある値）。
    pub ebwebview: Option<String>,
}

/// 見つかったランタイム。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub version: String,
    /// `EmbeddedBrowserWebView.dll` のパス。
    pub dll: PathBuf,
}

/// 版の文字列が使えるものか。数字だけの 2 つ以上の部分を `.` でつないだ形で、すべて 0 では
/// ない（`0.0.0.0` はアンインストール済みの印）。
pub fn is_usable_version(pv: &str) -> bool {
    let parts: Vec<&str> = pv.split('.').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
        && parts.iter().any(|p| p.bytes().any(|b| b != b'0'))
}

/// 既定のインストール先 `<ProgramFiles(x86)>\Microsoft\EdgeWebView\Application\<pv>`。
pub fn default_runtime_dir(program_files_x86: &Path, pv: &str) -> PathBuf {
    program_files_x86
        .join("Microsoft")
        .join("EdgeWebView")
        .join("Application")
        .join(pv)
}

/// ランタイムフォルダーから本体 DLL のパスを作る。
pub fn embedded_dll_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(EMBEDDED_DLL_RELATIVE)
}

/// レジストリの値から、本体 DLL の置き場所の候補を優先順に作る（純粋）。
///
/// 1. `EBWebView` 値（`...\EBWebView`）の `x64\EmbeddedBrowserWebView.dll`
/// 2. `location` をランタイムフォルダー自身とみなす
/// 3. `location` を `Application` フォルダーとみなして `<pv>` を足す
/// 4. 既定のインストール先（`program_files_x86` があれば）
///
/// 版（`pv`）が使えない（なし、`0.0.0.0`、形が不正）なら空。`location` などが絶対パスでない、
/// NUL や `..` を含むときは、その値を捨てる（レジストリの値は信頼しない）。
pub fn dll_candidates(entry: &RegistryEntry, program_files_x86: Option<&Path>) -> Vec<PathBuf> {
    let Some(pv) = entry
        .pv
        .as_deref()
        .map(str::trim)
        .filter(|p| is_usable_version(p))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(eb) = entry.ebwebview.as_deref().and_then(sane_path) {
        out.push(eb.join("x64").join("EmbeddedBrowserWebView.dll"));
    }
    if let Some(loc) = entry.location.as_deref().and_then(sane_path) {
        out.push(embedded_dll_path(&loc));
        out.push(embedded_dll_path(&loc.join(pv)));
    }
    if let Some(pf) = program_files_x86 {
        out.push(embedded_dll_path(&default_runtime_dir(pf, pv)));
    }
    out.dedup();
    out
}

/// 絶対パスで、NUL・`..` を含まないものだけを通す。
fn sane_path(s: &str) -> Option<PathBuf> {
    let s = s.trim().trim_end_matches(['\\', '/']);
    if s.is_empty() || s.contains('\0') || s.split(['\\', '/']).any(|c| c == "..") {
        return None;
    }
    let p = PathBuf::from(s);
    p.is_absolute().then_some(p)
}

/// 探索順に並んだエントリーから、`exists` が真を返す最初の DLL を選ぶ（純粋。実在の確認は
/// 呼び出し側が渡す）。
pub fn choose_runtime(
    entries: &[RegistryEntry],
    program_files_x86: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<RuntimeInfo> {
    entries.iter().find_map(|e| {
        dll_candidates(e, program_files_x86)
            .into_iter()
            .find(|p| exists(p))
            .map(|dll| RuntimeInfo {
                version: e.pv.as_deref().map(str::trim).unwrap_or("").to_owned(),
                dll,
            })
    })
}

/// `%LOCALAPPDATA%` を元にユーザーデータフォルダーを作る（純粋）。
pub fn user_data_dir_from(local_app_data: &Path) -> PathBuf {
    local_app_data.join(USER_DATA_RELATIVE)
}

/// ユーザーデータフォルダー（`%LOCALAPPDATA%\Linote\WebView2`）。作成は WebView2 が行う。
pub fn default_user_data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(|v| user_data_dir_from(Path::new(&v)))
}

// ---- ここから入出力 ----

fn read_reg_string(hive: HKEY, subkey: &str, value: PCWSTR) -> Option<String> {
    let sub = to_wide(subkey);
    let mut size = 0u32;
    // SAFETY: 引数は呼び出しの間有効。1 回目は大きさだけを取る。
    unsafe {
        let rc = RegGetValueW(
            hive,
            PCWSTR(sub.as_ptr()),
            value,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        );
        if rc != ERROR_SUCCESS || !(2..=64 * 1024).contains(&size) {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let mut size2 = (buf.len() * 2) as u32;
        let rc = RegGetValueW(
            hive,
            PCWSTR(sub.as_ptr()),
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size2),
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        (!s.is_empty()).then_some(s)
    }
}

/// レジストリから、探索順にエントリーを読む。
pub fn read_registry_entries() -> Vec<RegistryEntry> {
    REGISTRY_SOURCES
        .iter()
        .map(|(hive, base)| {
            let key = format!(r"{base}\{RUNTIME_CLIENT_GUID}");
            let h = match hive {
                Hive::LocalMachine => HKEY_LOCAL_MACHINE,
                Hive::CurrentUser => HKEY_CURRENT_USER,
            };
            RegistryEntry {
                pv: read_reg_string(h, &key, w!("pv")),
                location: read_reg_string(h, &key, w!("location")),
                ebwebview: read_reg_string(h, &key, w!("EBWebView")),
            }
        })
        .collect()
}

/// インストール済みの Evergreen ランタイムを探す。なければ [`Error::Unavailable`]。
pub fn find_runtime() -> Result<RuntimeInfo> {
    let pf86 = std::env::var_os("ProgramFiles(x86)")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let entries = read_registry_entries();
    choose_runtime(&entries, pf86.as_deref(), &|p| p.is_file()).ok_or_else(|| {
        Error::unavailable(
            "WebView2 ランタイム",
            "Microsoft Edge WebView2 Runtime が見つかりません",
        )
    })
}

type CreateEnvFn = unsafe extern "system" fn(
    check_running_instance: BOOL,
    runtime_type: i32,
    user_data_folder: PCWSTR,
    options: *mut c_void,
    handler: *mut c_void,
) -> HRESULT;

/// 読み込んだ本体 DLL の関数（プロセスの終わりまで解放しない）。
static CREATE_FN: OnceLock<usize> = OnceLock::new();

fn load_create_fn(runtime: &RuntimeInfo) -> Result<CreateEnvFn> {
    let addr = match CREATE_FN.get() {
        Some(&a) => a,
        None => {
            let wide = os_to_wide(runtime.dll.as_os_str());
            // SAFETY: パスは NUL 終端の絶対パス（LOAD_WITH_ALTERED_SEARCH_PATH の条件）。
            let module: HMODULE = unsafe {
                LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH)
            }
            .map_err(|e| {
                Error::unavailable(
                    "WebView2 ランタイム",
                    format!("{} を読み込めません: {e}", runtime.dll.display()),
                )
            })?;
            // SAFETY: module は有効なハンドル。名前は NUL 終端。
            let proc = unsafe {
                GetProcAddress(module, s!("CreateWebViewEnvironmentWithOptionsInternal"))
            };
            let Some(proc) = proc else {
                return Err(Error::unavailable(
                    "WebView2 ランタイム",
                    "CreateWebViewEnvironmentWithOptionsInternal が見つかりません（非対応の版）",
                ));
            };
            let a = proc as usize;
            let _ = CREATE_FN.set(a);
            a
        }
    };
    // SAFETY: 保存したのは GetProcAddress で得た、公開ローダーが呼ぶものと同じ署名の関数。
    Ok(unsafe { std::mem::transmute::<usize, CreateEnvFn>(addr) })
}

/// ランタイムを探して環境の生成を始める。完了は UI スレッドのメッセージループで
/// `on_done` に届く（呼び出しスレッドは COM のアパートメントを初期化済みであること）。
///
/// 失敗（ランタイムなし、DLL を読めない、呼び出しの失敗）はここで `Err` を返し、`on_done` は
/// 呼ばれない。
pub fn create_environment(
    user_data_dir: &Path,
    on_done: impl FnOnce(std::result::Result<ICoreWebView2Environment, windows::core::Error>) + 'static,
) -> Result<()> {
    let runtime = find_runtime()?;
    let create = load_create_fn(&runtime)?;
    let mut on_done = Some(on_done);
    let handler: ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler =
        CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
            if let Some(f) = on_done.take() {
                match (hr, env) {
                    (Ok(()), Some(env)) => f(Ok(env)),
                    (Err(e), _) => f(Err(e)),
                    // E_POINTER 相当（成功したのに環境がない）。
                    (Ok(()), None) => f(Err(windows::core::Error::from(HRESULT(
                        0x8000_4003u32 as i32,
                    )))),
                }
            }
            Ok(())
        }));
    let folder = to_wide(&user_data_dir.to_string_lossy());
    // SAFETY: 関数は上で検証した署名。ハンドラーは呼び出し先が AddRef して保持する。
    let hr = unsafe {
        create(
            BOOL(1),
            0,
            PCWSTR(folder.as_ptr()),
            std::ptr::null_mut(),
            handler.as_raw(),
        )
    };
    hr.ok()
        .map_err(|e| Error::unavailable("WebView2 ランタイム", format!("環境を作れません: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pv: Option<&str>, loc: Option<&str>, eb: Option<&str>) -> RegistryEntry {
        RegistryEntry {
            pv: pv.map(str::to_owned),
            location: loc.map(str::to_owned),
            ebwebview: eb.map(str::to_owned),
        }
    }

    #[test]
    fn version_validation() {
        assert!(is_usable_version("120.0.2210.91"));
        assert!(is_usable_version("1.2"));
        assert!(!is_usable_version("0.0.0.0"));
        assert!(!is_usable_version(""));
        assert!(!is_usable_version("1..2"));
        assert!(!is_usable_version("1.2.x"));
        assert!(!is_usable_version(r"1.2\..\3"));
        assert!(!is_usable_version("5"));
    }

    #[test]
    fn default_location_is_program_files_x86() {
        let e = entry(Some("120.0.1.2"), None, None);
        let c = dll_candidates(&e, Some(Path::new(r"C:\Program Files (x86)")));
        assert_eq!(
            c,
            vec![PathBuf::from(
                r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\120.0.1.2\EBWebView\x64\EmbeddedBrowserWebView.dll"
            )]
        );
    }

    #[test]
    fn location_is_tried_both_ways_before_default() {
        let e = entry(Some("9.8.7.6"), Some(r"D:\rt\Application\"), None);
        let c = dll_candidates(&e, Some(Path::new(r"C:\PF")));
        assert_eq!(c.len(), 3);
        assert!(c[0].ends_with(r"Application\EBWebView\x64\EmbeddedBrowserWebView.dll"));
        assert!(c[1].ends_with(r"Application\9.8.7.6\EBWebView\x64\EmbeddedBrowserWebView.dll"));
        assert!(c[2].starts_with(r"C:\PF"));
    }

    #[test]
    fn ebwebview_value_comes_first() {
        let e = entry(Some("1.2.3.4"), None, Some(r"E:\x\1.2.3.4\EBWebView"));
        let c = dll_candidates(&e, None);
        assert_eq!(
            c,
            vec![PathBuf::from(
                r"E:\x\1.2.3.4\EBWebView\x64\EmbeddedBrowserWebView.dll"
            )]
        );
    }

    #[test]
    fn unusable_or_hostile_entries_give_nothing() {
        let pf = Some(Path::new(r"C:\PF"));
        assert!(dll_candidates(&entry(None, Some(r"D:\a"), None), pf).is_empty());
        assert!(dll_candidates(&entry(Some("0.0.0.0"), Some(r"D:\a"), None), pf).is_empty());
        // 相対パスと `..` は捨てる（既定の場所だけが残る）。
        let c = dll_candidates(&entry(Some("1.2"), Some(r"rel\a"), Some(r"D:\a\..\b")), pf);
        assert_eq!(c.len(), 1);
        assert!(c[0].starts_with(r"C:\PF"));
    }

    #[test]
    fn choose_follows_source_order_and_existence() {
        let a = entry(Some("1.1.1.1"), Some(r"D:\a"), None);
        let b = entry(Some("2.2.2.2"), Some(r"D:\b"), None);
        let none = RegistryEntry::default();
        let only_b = |p: &Path| p.starts_with(r"D:\b");
        let got = choose_runtime(&[none.clone(), a.clone(), b.clone()], None, &only_b);
        assert_eq!(got.unwrap().version, "2.2.2.2");
        let all = |_: &Path| true;
        let got = choose_runtime(&[none.clone(), a, b], None, &all);
        assert_eq!(got.unwrap().version, "1.1.1.1");
        assert!(choose_runtime(&[none], None, &all).is_none());
    }

    #[test]
    fn user_data_dir_is_under_linote() {
        assert_eq!(
            user_data_dir_from(Path::new(r"C:\Users\u\AppData\Local")),
            PathBuf::from(r"C:\Users\u\AppData\Local\Linote\WebView2")
        );
    }

    #[test]
    fn registry_order_matches_architecture() {
        assert_eq!(REGISTRY_SOURCES[0].0, Hive::LocalMachine);
        assert!(REGISTRY_SOURCES[0].1.contains("WOW6432Node"));
        assert_eq!(REGISTRY_SOURCES[3].0, Hive::CurrentUser);
        assert!(!REGISTRY_SOURCES[3].1.contains("WOW6432Node"));
    }

    #[test]
    fn find_runtime_does_not_panic_without_runtime() {
        // Wine（WebView2 なし）では Unavailable、実機では Ok のどちらか。
        match find_runtime() {
            Ok(r) => assert!(r.dll.is_file()),
            Err(e) => assert_eq!(e.code(), "unavailable"),
        }
    }
}
