#![cfg_attr(not(test), windows_subsystem = "windows")]

//! Linote の起動入口。コマンドラインの解釈と、既存のウィンドウへの受け渡し（V8、
//! `linote::cli`・`linote::ui::app::launch`）、隠しオプション `--self-test`（画面の自己診断）を
//! 扱う。将来の `--mcp-stdio` の分岐もここに置く（要件 C、初版は未実装）。

#[cfg(windows)]
fn main() {
    use linote::ui::app::launch::{prepare, Prepared, SystemEnv};
    use linote::ui::app::{self, selftest};

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let code = match prepare(&args, &mut SystemEnv) {
        Prepared::Exit(code) => code,
        Prepared::SelfTest => selftest::run_self_test(),
        Prepared::Run(options) => app::run(options),
    };
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Linote は Windows 専用です。");
    std::process::exit(1);
}
