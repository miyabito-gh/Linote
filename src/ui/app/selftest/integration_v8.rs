//! V8: 起動オプション。起動の流れ（差し替えた環境で、ヘルプ・エラー・受け渡しの判断）、
//! 受け取り側（開く・既存タブへの切り替え・行と桁・読み取り専用・文字コード・標準入力）、
//! `WM_COPYDATA` での受け渡し（同じプロセスの窓へ送る）、巨大文書の行への移動。

use std::ffi::OsString;
use std::path::PathBuf;

use super::integration::{cmd, open_and_wait, step, wait};
use super::integration_i2::{find_big, write_numbered_lines};
use super::{text_of, Check, Ctx};
use crate::bigfile::LargeModeThresholds;
use crate::cli::{self, FileSpec, Forward};
use crate::core::DocumentId;
use crate::document::Capability;
use crate::text::{Encoding, Eol};
use crate::ui::app::launch::{self, LaunchEnv, Prepared};

macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!("{}（{}:{}）", format!($($msg)+), file!(), line!()));
        }
    };
}

pub(super) fn run(ctx: &Ctx) -> Check {
    prepare_flow(ctx)?;
    receive_files(ctx)?;
    receive_stdin()?;
    copy_data(ctx)?;
    big_file(ctx)?;
    Ok(())
}

/// 差し替えた環境（ダイアログを出さず、記録する）。
struct TestEnv {
    cwd: PathBuf,
    notices: Vec<(String, bool)>,
    stdin: Result<Vec<u8>, String>,
    reuse_setting: bool,
    accept: bool,
    forwarded: Vec<(Vec<FileSpec>, Option<Vec<u8>>)>,
}

impl TestEnv {
    fn new(cwd: PathBuf) -> Self {
        TestEnv {
            cwd,
            notices: Vec::new(),
            stdin: Ok(b"in".to_vec()),
            reuse_setting: true,
            accept: false,
            forwarded: Vec::new(),
        }
    }
}

impl LaunchEnv for TestEnv {
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn notice(&mut self, _title: &str, text: &str, is_error: bool) {
        self.notices.push((text.to_owned(), is_error));
    }
    fn read_stdin(&mut self) -> Result<Vec<u8>, String> {
        self.stdin.clone()
    }
    fn open_in_existing_window(&mut self) -> bool {
        self.reuse_setting
    }
    fn forward(&mut self, files: &[FileSpec], stdin: Option<&[u8]>) -> bool {
        self.forwarded
            .push((files.to_vec(), stdin.map(<[u8]>::to_vec)));
        self.accept
    }
}

fn os(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn prepare_flow(ctx: &Ctx) -> Check {
    let mut env = TestEnv::new(ctx.dir.clone());
    // ヘルプ・版・不明なオプション・数値エラー: ダイアログ（記録）を出して終了。
    for (args, code, is_error, needle) in [
        (&["--help"][..], 0, false, "使い方"),
        (&["/?"], 0, false, "使い方"),
        (&["-v"], 0, false, "Linote "),
        (&["--bogus"], 2, true, "不明なオプション: --bogus"),
        (&["-n", "0", "a"], 2, true, "1 以上"),
    ] {
        env.notices.clear();
        match launch::prepare(&os(args), &mut env) {
            Prepared::Exit(c) => check!(c == code, "{args:?} の終了コード {c}"),
            _ => return Err(format!("{args:?} は終了するはず")),
        }
        check!(
            env.notices.len() == 1
                && env.notices[0].1 == is_error
                && env.notices[0].0.contains(needle),
            "{args:?} の表示 {:?}",
            env.notices
        );
    }
    check!(env.forwarded.is_empty(), "表示だけの起動では渡さない");
    check!(
        matches!(
            launch::prepare(&os(&["--self-test"]), &mut env),
            Prepared::SelfTest
        ),
        "自己診断"
    );

    // 渡す: 受け取られたら終了。相対パスは cwd で絶対にして渡す。
    env.accept = true;
    match launch::prepare(&os(&["-n", "7", "rel.txt"]), &mut env) {
        Prepared::Exit(0) => {}
        _ => return Err("受け取られたら終了するはず".into()),
    }
    check!(
        env.forwarded.len() == 1
            && env.forwarded[0].0[0].path == ctx.dir.join("rel.txt")
            && env.forwarded[0].0[0].line == Some(7),
        "渡した内容 {:?}",
        env.forwarded
    );
    // 受け取られなければ自分で開く（指定はそのまま）。
    env.accept = false;
    env.forwarded.clear();
    match launch::prepare(&os(&["-r", "rel.txt"]), &mut env) {
        Prepared::Run(o) => {
            check!(o.files.len() == 1 && o.files[0].read_only, "自分で開く指定");
            check!(!o.no_session && o.stdin.is_none(), "復元は有効のまま");
        }
        _ => return Err("受け取られなければ自分で開くはず".into()),
    }
    check!(env.forwarded.len() == 1, "一度は渡そうとした");
    // 新しいウィンドウ・設定が偽・--adopt では渡さない。--reuse-window は設定にかかわらず渡す。
    env.forwarded.clear();
    for args in [&["-w", "a"][..], &["--adopt", "h.json"]] {
        check!(
            matches!(launch::prepare(&os(args), &mut env), Prepared::Run(_)),
            "{args:?}"
        );
    }
    env.reuse_setting = false;
    check!(
        matches!(launch::prepare(&os(&["a"]), &mut env), Prepared::Run(_)),
        "設定が偽なら渡さない"
    );
    check!(env.forwarded.is_empty(), "渡していない {:?}", env.forwarded);
    env.accept = true;
    check!(
        matches!(
            launch::prepare(&os(&["--reuse-window", "a"]), &mut env),
            Prepared::Exit(0)
        ),
        "--reuse-window"
    );
    // 何も指定しない起動も、渡せれば前面に出すだけで終了する。
    env.forwarded.clear();
    env.reuse_setting = true;
    check!(
        matches!(launch::prepare(&[], &mut env), Prepared::Exit(0)),
        "引数なし"
    );
    check!(
        env.forwarded.len() == 1 && env.forwarded[0].0.is_empty() && env.forwarded[0].1.is_none(),
        "空の要求"
    );
    // 標準入力: 読んで渡す。読めなければ知らせ、復元もしない。
    env.forwarded.clear();
    match launch::prepare(&os(&["-"]), &mut env) {
        Prepared::Exit(0) => {}
        _ => return Err("標準入力を渡して終了するはず".into()),
    }
    check!(
        env.forwarded[0].1.as_deref() == Some(&b"in"[..]),
        "標準入力の内容"
    );
    env.accept = false;
    env.notices.clear();
    env.stdin = Err("端末".into());
    match launch::prepare(&os(&["-"]), &mut env) {
        Prepared::Run(o) => check!(o.stdin.is_none() && o.no_session, "読めなかった標準入力"),
        _ => return Err("読めなくても起動する".into()),
    }
    check!(
        env.notices.len() == 1 && env.notices[0].1 && env.notices[0].0.contains("端末"),
        "読めない知らせ {:?}",
        env.notices
    );
    Ok(())
}

fn close_all() -> Check {
    step(|app| {
        let ids: Vec<DocumentId> = app.documents().iter().map(|d| d.id()).collect();
        for id in ids {
            let _ = app.close_document(id, true);
        }
    })
}

fn caret_line_col(id: DocumentId) -> Result<(u64, u64), String> {
    step(|app| {
        app.activate(id).ok();
        let ed = app.editor();
        let caret = ed.selection().caret;
        (ed.line_from_position(caret) + 1, ed.column_chars(caret) + 1)
    })
}

fn path_doc(path: &std::path::Path) -> Result<Option<DocumentId>, String> {
    step(|app| {
        app.documents()
            .iter()
            .find(|d| d.state.path() == Some(path))
            .map(|d| d.id())
    })
}

fn receive_files(ctx: &Ctx) -> Check {
    close_all()?;
    let text: String = (1..=200).map(|i| format!("行{i} あいうえお\r\n")).collect();
    let a = ctx.dir.join("v8-a.txt");
    let b = ctx.dir.join("v8-b.txt");
    std::fs::write(&a, &text).map_err(|e| e.to_string())?;
    std::fs::write(&b, "b\r\n").map_err(|e| e.to_string())?;

    // 開く: 行・桁へ移動し、中ほどに見える。b は読み取り専用。
    let spec_a = FileSpec {
        line: Some(120),
        column: Some(3),
        ..FileSpec::new(a.clone())
    };
    let spec_b = FileSpec {
        read_only: true,
        ..FileSpec::new(b.clone())
    };
    step(|app| {
        app.receive_forward(Forward {
            files: vec![spec_a.clone(), spec_b.clone()],
            stdin: None,
        })
    })?;
    wait("受け取ったファイルが開く", |app| {
        app.documents().len() == 2
    })?;
    let ida = path_doc(&a)?.ok_or("a がない")?;
    let idb = path_doc(&b)?.ok_or("b がない")?;
    check!(
        caret_line_col(ida)? == (120, 3),
        "移動先 {:?}",
        caret_line_col(ida)?
    );
    step(|app| -> Check {
        let first = app.editor().view_state().first_visible_line;
        check!(
            first > 60 && first < 120,
            "行 120 が画面の中ほどにある（先頭行 {first}）"
        );
        Ok(())
    })??;
    step(|app| -> Check {
        let d = app.document(idb).ok_or("b")?;
        check!(d.state.is_read_only(), "b は読み取り専用");
        check!(
            !d.state.capabilities().contains(Capability::Edit),
            "編集不可"
        );
        check!(
            !d.state.capabilities().contains(Capability::Save),
            "上書き保存不可"
        );
        let da = app.document(ida).ok_or("a")?;
        check!(!da.state.is_read_only(), "a は編集できる");
        Ok(())
    })??;
    // 読み取り専用の編集欄: 入力しても本文が変わらない。
    step(|app| -> Check {
        app.activate(idb).map_err(super::e2s)?;
        check!(app.editor().is_read_only(), "編集欄が読み取り専用");
        Ok(())
    })??;
    cmd("file.save", crate::command::Value::Null)?;
    step(|app| -> Check {
        check!(text_of(app, idb) == "b\r\n", "本文が変わらない");
        Ok(())
    })??;

    // 既に開いている a を別の行で受け取る: 新しいタブを作らず、切り替えて移動する。
    step(|app| app.activate(idb).map_err(super::e2s))??;
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                line: Some(5),
                column: Some(1),
                ..FileSpec::new(a.clone())
            }],
            stdin: None,
        })
    })?;
    check!(
        step(|app| app.documents().len())? == 2 && step(|app| app.active_id())? == Some(ida),
        "既存のタブへ切り替える"
    );
    check!(caret_line_col(ida)? == (5, 1), "既存のタブで行 5");
    // 範囲外の行・桁は末尾に収める。
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                line: Some(9999),
                column: Some(9999),
                ..FileSpec::new(a.clone())
            }],
            stdin: None,
        })
    })?;
    let (line, _) = caret_line_col(ida)?;
    check!(line >= 200, "末尾に収める（行 {line}）");
    // 既に開いている文書を読み取り専用で受け取ると、読み取り専用になる。
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                read_only: true,
                ..FileSpec::new(a.clone())
            }],
            stdin: None,
        })
    })?;
    check!(
        step(|app| app.document(ida).is_some_and(|d| d.state.is_read_only()))?,
        "既存の文書も読み取り専用にできる"
    );

    // 文字コード: 新規に開く。
    let sj = ctx.dir.join("v8-sjis.txt");
    std::fs::write(&sj, b"\x82\xa0\x82\xa2\r\nline2\r\n").map_err(|e| e.to_string())?;
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                encoding: Some((Encoding::Cp932, false)),
                line: Some(2),
                ..FileSpec::new(sj.clone())
            }],
            stdin: None,
        })
    })?;
    wait("文字コードを指定して開く", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path() == Some(sj.as_path()))
    })?;
    let ids = path_doc(&sj)?.ok_or("sjis")?;
    step(|app| -> Check {
        check!(text_of(app, ids) == "あい\r\nline2\r\n", "CP932 で読めた");
        check!(
            app.state(ids).unwrap().format.encoding == Encoding::Cp932,
            "形式"
        );
        Ok(())
    })??;
    check!(caret_line_col(ids)?.0 == 2, "文字コード指定でも行へ移動");
    // 既に開いている UTF-8 のファイルへ指定の文字コードで開き直す（未保存の変更なし）。
    let u8file = ctx.dir.join("v8-u8.txt");
    std::fs::write(&u8file, "あ\r\nz\r\n").map_err(|e| e.to_string())?;
    let idu = open_and_wait(&u8file)?;
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                encoding: Some((Encoding::Cp932, false)),
                line: Some(2),
                ..FileSpec::new(u8file.clone())
            }],
            stdin: None,
        })
    })?;
    wait("開き直しが終わる", |app| {
        app.documents()
            .iter()
            .any(|d| d.state.path() == Some(u8file.as_path()) && d.id() != idu)
    })?;
    let idu2 = path_doc(&u8file)?.ok_or("開き直した文書")?;
    step(|app| -> Check {
        check!(
            app.state(idu2).unwrap().format.encoding == Encoding::Cp932,
            "指定の文字コードで開き直した"
        );
        check!(
            !text_of(app, idu2).starts_with("あ"),
            "別の文字コードで読んだ"
        );
        Ok(())
    })??;
    check!(caret_line_col(idu2)?.0 == 2, "開き直しでも行へ移動");

    // 読み取り専用で開いた文書は、文字コードを指定して開き直しても読み取り専用のまま。
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                encoding: Some((Encoding::Utf8, false)),
                ..FileSpec::new(b.clone())
            }],
            stdin: None,
        })
    })?;
    wait("読み取り専用の開き直し", |app| {
        app.documents().iter().any(|d| {
            d.state.path() == Some(b.as_path())
                && d.state.format.encoding == Encoding::Utf8
                && d.state.id() != idb
        })
    })?;
    let idb2 = path_doc(&b)?.ok_or("b2")?;
    check!(
        step(|app| app.document(idb2).is_some_and(|d| d.state.is_read_only()))?,
        "開き直しても読み取り専用"
    );

    // 存在しないファイル・フォルダー: 指定を残さず、知らせるだけ。
    let before = step(|app| app.documents().len())?;
    let missing = ctx.dir.join("v8-missing.txt");
    step(|app| {
        app.receive_forward(Forward {
            files: vec![
                FileSpec {
                    line: Some(3),
                    read_only: true,
                    encoding: Some((Encoding::Cp932, false)),
                    ..FileSpec::new(missing.clone())
                },
                FileSpec::new(ctx.dir.clone()),
            ],
            stdin: None,
        })
    })?;
    check!(
        step(|app| app.documents().len())? == before,
        "開けないものは増えない"
    );
    check!(
        step(|app| app.pending_jumps.is_empty()
            && app.pending_read_only.is_empty()
            && app.pending_encodings.is_empty())?,
        "待たせる指定が残らない"
    );
    // 後で同じパスを作って普通に開くと、普通に開く。
    std::fs::write(&missing, "m\r\n").map_err(|e| e.to_string())?;
    let idm = open_and_wait(&missing)?;
    check!(
        step(|app| app.document(idm).is_some_and(|d| !d.state.is_read_only()))?,
        "古い指定が残っていない"
    );
    close_all()?;
    Ok(())
}

fn receive_stdin() -> Check {
    close_all()?;
    // 標準入力: 文字コードは自動判定（UTF-8、LF）。無題の未保存の文書として開く。
    step(|app| {
        app.settings.new_file.format.eol = Eol::CrLf;
        app.receive_forward(Forward {
            files: vec![],
            stdin: Some("diff --git\n+日本語\n".as_bytes().to_vec()),
        })
    })?;
    wait("標準入力の文書が開く", |app| {
        app.documents().len() == 1
    })?;
    step(|app| -> Check {
        let id = app.active_id().ok_or("アクティブな文書がない")?;
        let d = app.document(id).unwrap();
        check!(d.state.path().is_none(), "パスなし");
        check!(d.state.is_dirty(), "未保存として扱う");
        check!(d.state.format.encoding == Encoding::Utf8, "UTF-8");
        check!(d.state.format.eol == Eol::Lf, "改行は内容に従う");
        check!(text_of(app, id) == "diff --git\n+日本語\n", "本文");
        Ok(())
    })??;
    // Shift_JIS の標準入力も判定される。
    step(|app| {
        app.receive_forward(Forward {
            files: vec![],
            stdin: Some(b"\x82\xa0\x82\xa2\x82\xa4\x82\xa6\x82\xa8\r\n".to_vec()),
        })
    })?;
    wait("2 つ目の標準入力", |app| app.documents().len() == 2)?;
    step(|app| -> Check {
        let id = app.active_id().ok_or("アクティブな文書がない")?;
        check!(text_of(app, id) == "あいうえお\r\n", "CP932 の判定");
        Ok(())
    })??;
    // ファイルも標準入力もない要求: 文書を増やさず、前面に出すだけ。
    step(|app| app.receive_forward(Forward::default()))?;
    check!(step(|app| app.documents().len())? == 2, "何も増えない");
    // 0 件のウィンドウでも受け取れる。
    close_all()?;
    step(|app| {
        app.receive_forward(Forward {
            files: vec![],
            stdin: Some(b"x".to_vec()),
        })
    })?;
    wait("0 件からの標準入力", |app| {
        app.documents().len() == 1
    })?;
    close_all()?;
    Ok(())
}

/// 同じプロセスの窓へ `WM_COPYDATA` で送る（送る側は、アプリを借りていない状態で呼ぶ）。
fn copy_data(ctx: &Ctx) -> Check {
    close_all()?;
    let hwnd = step(|app| app.hwnd())?;
    // 探索: 自分のプロセスを除けば見つからず、除かなければ自分の窓が見つかる。
    check!(
        launch::find_main_windows(None).contains(&hwnd),
        "メインウィンドウを探せる"
    );
    check!(
        !launch::find_main_windows(Some(std::process::id())).contains(&hwnd),
        "自分のプロセスは除く"
    );
    check!(
        !launch::forward_to_existing_window(&[], None),
        "自分以外にウィンドウがなければ渡せない"
    );
    // 壊れた本体・知らない版は受け取らない（0 が返る）。
    check!(!launch::send_forward(hwnd, b"garbage"), "壊れた本体");
    check!(
        !launch::send_forward(hwnd, b"{\"version\":99,\"files\":[],\"stdin\":false}\0"),
        "未来の版"
    );
    let path = ctx.dir.join("v8-copydata.txt");
    std::fs::write(&path, "1\r\n2\r\n3\r\n").map_err(|e| e.to_string())?;
    let data = cli::encode_forward(
        &[FileSpec {
            line: Some(2),
            read_only: true,
            ..FileSpec::new(path.clone())
        }],
        Some("stdin 本文".as_bytes()),
    );
    check!(launch::send_forward(hwnd, &data), "受け取られた");
    wait("WM_COPYDATA で渡した文書が開く", |app| {
        app.documents().len() == 2
    })?;
    let id = path_doc(&path)?.ok_or("渡したファイルがない")?;
    check!(caret_line_col(id)?.0 == 2, "行 2");
    check!(
        step(|app| app.document(id).is_some_and(|d| d.state.is_read_only()))?,
        "読み取り専用"
    );
    check!(
        step(|app| app.documents().iter().any(|d| d.state.path().is_none()))?,
        "標準入力の無題の文書"
    );
    // 渡した後に新しく何かが勝手に作られていない（無題は標準入力の 1 つだけ）。
    close_all()?;
    Ok(())
}

fn big_file(ctx: &Ctx) -> Check {
    step(|app| {
        app.large_thresholds = LargeModeThresholds {
            max_bytes: 16 * 1024,
            max_lines: 1_000_000,
            max_line_bytes: 1 << 20,
        }
    })?;
    let path = ctx.dir.join("v8-big.log");
    write_numbered_lines(&path, 3000)?;
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                line: Some(1500),
                column: Some(4),
                read_only: true,
                encoding: Some((Encoding::Cp932, false)),
                ..FileSpec::new(path.clone())
            }],
            stdin: None,
        })
    })?;
    let p = path.clone();
    wait("巨大ファイルが開く", move |app| {
        find_big(app, &p).is_some()
    })?;
    let id = step(|app| find_big(app, &path).unwrap())?;
    wait("行索引ができる", |app| {
        app.bigviews
            .get(&id)
            .and_then(|b| b.view.status())
            .is_some_and(|s| s.index.complete)
    })?;
    let line = step(|app| {
        app.bigviews
            .get(&id)
            .and_then(|b| b.view.status())
            .and_then(|s| s.line)
    })?;
    check!(line == Some(1500), "巨大ファイルの行 {line:?}");
    // 既に開いている巨大ファイルへ別の行。
    step(|app| {
        app.receive_forward(Forward {
            files: vec![FileSpec {
                line: Some(2000),
                ..FileSpec::new(path.clone())
            }],
            stdin: None,
        })
    })?;
    let line = step(|app| {
        app.bigviews
            .get(&id)
            .and_then(|b| b.view.status())
            .and_then(|s| s.line)
    })?;
    check!(line == Some(2000), "既存の巨大ファイルの行 {line:?}");
    check!(
        step(|app| app.pending_read_only.is_empty())?,
        "指定が残らない"
    );
    step(|app| app.large_thresholds = LargeModeThresholds::DEFAULT)?;
    close_all()?;
    Ok(())
}
