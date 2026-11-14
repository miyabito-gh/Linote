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
    // `--adopt <ファイル> [--pos x,y]`: 別のウィンドウから移された文書を受け取る（U4）。
    let mut adopt = None;
    let mut window_pos = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--adopt" {
            adopt = it.next().map(PathBuf::from);
        } else if a == "--pos" {
            window_pos = it.next().and_then(|s| {
                s.to_str()
                    .and_then(linote::appdata::handoff::parse_position)
            });
        } else {
            files.push(PathBuf::from(a));
        }
    }
    let code = app::run(RunOptions {
        files,
        adopt,
        window_pos,
        ..RunOptions::default()
    });
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Linote は Windows 専用です。");
    std::process::exit(1);
}
