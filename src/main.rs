#![cfg_attr(not(test), windows_subsystem = "windows")]

//! Linote の起動入口。GUI の起動と、隠しオプション `--self-test`（画面の自己診断）を扱う。
//! 将来の `--mcp-stdio` の分岐もここに置く（要件 C、初版は未実装）。

#[cfg(windows)]
fn main() {
    use linote::ui::app::{self, selftest, RunOptions};
    use std::path::PathBuf;

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--self-test") {
        std::process::exit(selftest::run_self_test());
    }
    let files: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    let code = app::run(RunOptions {
        files,
        ..RunOptions::default()
    });
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Linote は Windows 専用です。");
    std::process::exit(1);
}
