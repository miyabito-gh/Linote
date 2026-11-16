//! コマンドラインの解釈（V8）。GUI に依存しない純粋な処理で、Linux でも単体テストできる。
//!
//! 引数を [`Launch`]（起動要求）にする。相対パスは、起動したプロセスのカレントフォルダー
//! （`cwd`）で絶対パスにする（既存のウィンドウへ渡すと、受け取る側のカレントフォルダーが
//! 違うため）。既存のウィンドウへ渡すときの形式（[`encode_forward`]／[`decode_forward`]）も
//! ここにある。
//!
//! # 文法
//!
//! - `-n <行>`／`-n<行>`／`--line <行>`／`--line=<行>`／`+<行>`: 以降のファイルのカーソル行
//! - `-c <桁>`／`-c<桁>`／`--column <桁>`: 以降のファイルのカーソルの桁（文字単位）
//! - `-g <ファイル>[:<行>[:<桁>]]`／`--goto ...`: そのファイルだけ行・桁を指定。末尾から
//!   数字だけの `:` 区切りを最大 2 つ切り出すので、`C:\a.txt:10:5` のドライブ文字の `:` は
//!   影響しない。
//! - `-r`／`--readonly`: 以降のファイルを読み取り専用で開く
//! - `-e <名前>`／`--encoding <名前>`: 以降のファイルを指定の文字コードで開く
//!   （名前は [`parse_new_file_encoding`] と同じ）
//! - `-`: 標準入力を読んで無題の文書として開く
//! - `-w`／`--new-window`、`--reuse-window`、`--no-session`
//! - `-h`／`--help`／`/?`、`-v`／`--version`、`--`（以降はすべてファイル名）
//! - 内部用: `--adopt <ファイル>`、`--pos x,y`、`--self-test`
//!
//! `-n`／`-c`／`-r`／`-e` は、それより後に書いたファイルすべてに適用する（同じオプションを
//! もう一度書くと上書き）。数値は 1 以上の整数。知らないオプションはエラー。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::appdata::settings::{new_file_encoding_name, parse_new_file_encoding};
use crate::text::{Encoding, Eol, TextFormat};

/// 開くファイル 1 つ分の指定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    /// 絶対パス。
    pub path: PathBuf,
    /// カーソルの行（1 始まり）。
    pub line: Option<u64>,
    /// カーソルの桁（1 始まり、文字単位）。
    pub column: Option<u64>,
    /// 指定された文字コードと、UTF-8 の BOM の有無。
    pub encoding: Option<(Encoding, bool)>,
    pub read_only: bool,
}

impl FileSpec {
    pub fn new(path: PathBuf) -> Self {
        FileSpec {
            path,
            line: None,
            column: None,
            encoding: None,
            read_only: false,
        }
    }
}

/// ウィンドウの選び方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowChoice {
    /// 設定 `open_in_existing_window` に従う。
    #[default]
    Default,
    /// 既存のウィンドウに渡さず、新しいウィンドウで開く（`-w`）。
    New,
    /// 設定にかかわらず既存のウィンドウに渡す（`--reuse-window`）。
    Reuse,
}

/// 表示して終了するもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Info {
    Help,
    Version,
}

/// 起動要求。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
    pub files: Vec<FileSpec>,
    /// 標準入力を読んで開く（`-`）。
    pub stdin: bool,
    pub window: WindowChoice,
    /// 前回のタブを開き直さない。
    pub no_session: bool,
    pub info: Option<Info>,
    /// `--adopt`（別のウィンドウから移された文書の受け渡しファイル）。
    pub adopt: Option<PathBuf>,
    /// `--pos`（ウィンドウの置き場所の目安）。
    pub pos: Option<(i32, i32)>,
    /// `--self-test`。
    pub self_test: bool,
}

impl Launch {
    /// 既存のウィンドウに渡す対象（ファイルか標準入力）があるか。
    pub fn has_payload(&self) -> bool {
        !self.files.is_empty() || self.stdin
    }

    /// 既存のウィンドウへ渡してよい種類の起動か（`--new-window`・`--adopt`・`--self-test`・
    /// 表示だけの起動は渡さない）。設定の有無は別に判断する。
    pub fn may_forward(&self) -> bool {
        self.window != WindowChoice::New
            && self.adopt.is_none()
            && !self.self_test
            && self.info.is_none()
    }
}

/// 解釈の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// 知らないオプション。
    UnknownOption(String),
    /// 値が要るオプションに値がない。
    MissingValue(String),
    /// 数値が不正（0、負、数字でない）。
    BadNumber { option: String, value: String },
    /// 文字コードの名前が不正。
    BadEncoding(String),
    /// `-g` の指定にファイル名がない。
    BadGoto(String),
}

impl CliError {
    /// 利用者に見せる 1 行。
    pub fn message(&self) -> String {
        match self {
            CliError::UnknownOption(o) => format!("不明なオプション: {o}"),
            CliError::MissingValue(o) => format!("オプション {o} には値が必要です"),
            CliError::BadNumber { option, value } => {
                format!("オプション {option} の値は 1 以上の整数で指定してください: {value}")
            }
            CliError::BadEncoding(v) => format!(
                "文字コードを解釈できません: {v}（utf-8、utf-8-bom、shift_jis、utf-16le、utf-16be）"
            ),
            CliError::BadGoto(v) => format!("-g にはファイル名が必要です: {v}"),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// 使い方の文章。
pub fn usage() -> String {
    "使い方: linote [オプション] [ファイル ...]\n\
     \n\
     -n <行>, --line <行>, +<行>     以降のファイルのカーソル行（1 始まり）\n\
     -c <桁>, --column <桁>          以降のファイルのカーソルの桁（1 始まり）\n\
     -g <ファイル>:<行>[:<桁>]       そのファイルだけ行・桁を指定\n\
     -r, --readonly                  以降のファイルを読み取り専用で開く\n\
     -e <名前>, --encoding <名前>    以降のファイルを指定の文字コードで開く\n\
     -                               標準入力を読んで無題の文書として開く\n\
     -w, --new-window                開いているウィンドウに渡さず、新しいウィンドウで開く\n\
     --reuse-window                  設定にかかわらず、開いているウィンドウで開く\n\
     --no-session                    前回のタブを開き直さない\n\
     -h, --help, /?                  この説明を表示\n\
     -v, --version                   版を表示\n\
     --                              以降はすべてファイル名\n\
     \n\
     例:\n\
     linote -n 120 a.txt\n\
     linote -g \"C:\\src\\main.rs:42:7\"\n\
     git diff | linote -\n\
     linote -w"
        .to_owned()
}

/// 版の文章。
pub fn version_text() -> String {
    format!("Linote {}", env!("CARGO_PKG_VERSION"))
}

/// `s` が数字だけか。
fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `-g` の値 `ファイル[:行[:桁]]` を（ファイル名, 行, 桁）に分ける。末尾から数字だけの
/// `:` 区切りを最大 2 つ切り出す（ドライブ文字の `:` は数字でないので残る）。切り出した
/// 後のファイル名が空になる指定は分けずに全体をファイル名とする。
pub fn split_goto(spec: &str) -> (&str, Option<&str>, Option<&str>) {
    let mut rest = spec;
    let mut numbers: Vec<&str> = Vec::new();
    while numbers.len() < 2 {
        match rest.rsplit_once(':') {
            Some((head, tail)) if is_digits(tail) && !head.is_empty() => {
                numbers.push(tail);
                rest = head;
            }
            _ => break,
        }
    }
    match numbers.as_slice() {
        [] => (spec, None, None),
        [line] => (rest, Some(line), None),
        [column, line] => (rest, Some(line), Some(column)),
        _ => unreachable!("最大 2 つ"),
    }
}

fn parse_number(option: &str, value: &str) -> Result<u64, CliError> {
    match value.parse::<u64>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(CliError::BadNumber {
            option: option.to_owned(),
            value: value.to_owned(),
        }),
    }
}

fn resolve(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// 以降のファイルに適用する指定。
#[derive(Default)]
struct Current {
    line: Option<u64>,
    column: Option<u64>,
    encoding: Option<(Encoding, bool)>,
    read_only: bool,
}

impl Current {
    fn spec(&self, path: PathBuf) -> FileSpec {
        FileSpec {
            path,
            line: self.line,
            column: self.column,
            encoding: self.encoding,
            read_only: self.read_only,
        }
    }
}

/// コマンドライン（実行ファイル名を除く）を解釈する。相対パスは `cwd` で絶対にする。
pub fn parse(args: &[OsString], cwd: &Path) -> Result<Launch, CliError> {
    let mut launch = Launch::default();
    let mut cur = Current::default();
    let mut only_files = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        // UTF-8 として読めない引数は、オプションではありえないのでファイル名とする。
        let Some(s) = arg.to_str() else {
            launch
                .files
                .push(cur.spec(resolve(cwd, Path::new(arg.as_os_str()))));
            continue;
        };
        if only_files {
            launch.files.push(cur.spec(resolve(cwd, Path::new(s))));
            continue;
        }
        if s == "--" {
            only_files = true;
            continue;
        }
        if s == "-" {
            launch.stdin = true;
            continue;
        }
        if s == "/?" {
            launch.info = Some(Info::Help);
            return Ok(launch);
        }
        // 値を取る側の共通処理: (名前, 値が同じ引数に付いていればその値)。
        let (name, inline): (String, Option<&str>) = if let Some(long) = s.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v)),
                None => (s.to_owned(), None),
            }
        } else if s.len() >= 2 && s.starts_with('-') {
            if !s.is_char_boundary(2) {
                return Err(CliError::UnknownOption(s.to_owned()));
            }
            let (head, tail) = s.split_at(2);
            (head.to_owned(), (!tail.is_empty()).then_some(tail))
        } else if let Some(n) = s.strip_prefix('+').filter(|n| is_digits(n)) {
            cur.line = Some(parse_number("+", n)?);
            continue;
        } else {
            launch.files.push(cur.spec(resolve(cwd, Path::new(s))));
            continue;
        };

        // 値を取るオプションなら値を得る（次の引数を使う場合は読み進める）。
        let value = |i: &mut usize| -> Result<String, CliError> {
            if let Some(v) = inline {
                return Ok(v.to_owned());
            }
            match args.get(*i).and_then(|a| a.to_str()) {
                Some(v) => {
                    *i += 1;
                    Ok(v.to_owned())
                }
                None => Err(CliError::MissingValue(name.clone())),
            }
        };
        // 値を取らないオプションに値が付いていたら、知らないオプションとする。
        let flag_only = || inline.is_none();
        match name.as_str() {
            "-n" | "--line" => cur.line = Some(parse_number(&name, &value(&mut i)?)?),
            "-c" | "--column" => cur.column = Some(parse_number(&name, &value(&mut i)?)?),
            "-e" | "--encoding" => {
                let v = value(&mut i)?;
                cur.encoding = Some(parse_new_file_encoding(&v).ok_or(CliError::BadEncoding(v))?);
            }
            "-g" | "--goto" => {
                let v = value(&mut i)?;
                let (file, line, column) = split_goto(&v);
                if file.is_empty() {
                    return Err(CliError::BadGoto(v));
                }
                let mut spec = cur.spec(resolve(cwd, Path::new(file)));
                spec.line = line.map(|l| parse_number(&name, l)).transpose()?;
                spec.column = column.map(|c| parse_number(&name, c)).transpose()?;
                launch.files.push(spec);
            }
            "-r" | "--readonly" if flag_only() => cur.read_only = true,
            "-w" | "--new-window" if flag_only() => launch.window = WindowChoice::New,
            "--reuse-window" if flag_only() => launch.window = WindowChoice::Reuse,
            "--no-session" if flag_only() => launch.no_session = true,
            "-h" | "--help" if flag_only() => {
                launch.info = Some(Info::Help);
                return Ok(launch);
            }
            "-v" | "--version" if flag_only() => {
                launch.info = Some(Info::Version);
                return Ok(launch);
            }
            "--self-test" if flag_only() => launch.self_test = true,
            "--adopt" => launch.adopt = Some(resolve(cwd, Path::new(&value(&mut i)?))),
            "--pos" => {
                launch.pos = crate::appdata::handoff::parse_position(&value(&mut i)?);
            }
            _ => return Err(CliError::UnknownOption(s.to_owned())),
        }
    }
    Ok(launch)
}

// ------------------------------------------------------------------ 既存のウィンドウへの受け渡し

/// 受け渡しの形式の版。
pub const FORWARD_VERSION: u64 = 1;
/// `WM_COPYDATA` の識別子（`dwData`）。
pub const COPYDATA_ID: usize = 0x4C4E_5401;
/// これを超える受け渡しはせず、自分で開く。
pub const FORWARD_MAX_BYTES: usize = 64 * 1024 * 1024;

/// 既存のウィンドウが受け取る要求。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Forward {
    pub files: Vec<FileSpec>,
    /// 標準入力の内容（`-` の指定があったときだけ）。
    pub stdin: Option<Vec<u8>>,
}

fn encoding_json(encoding: Option<(Encoding, bool)>) -> Value {
    match encoding {
        Some((encoding, bom)) => json!(new_file_encoding_name(&TextFormat {
            encoding,
            bom,
            eol: Eol::CrLf,
        })),
        None => Value::Null,
    }
}

/// 受け渡しの本体: JSON、NUL 1 バイト、標準入力の生のバイト列（JSON に NUL は現れない）。
pub fn encode_forward(files: &[FileSpec], stdin: Option<&[u8]>) -> Vec<u8> {
    let list: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({
                "path": f.path.to_string_lossy(),
                "line": f.line,
                "column": f.column,
                "encoding": encoding_json(f.encoding),
                "read_only": f.read_only,
            })
        })
        .collect();
    let head = json!({
        "version": FORWARD_VERSION,
        "files": list,
        "stdin": stdin.is_some(),
    });
    let mut out = serde_json::to_vec(&head).expect("JSON の値は常に書き出せる");
    out.push(0);
    if let Some(bytes) = stdin {
        out.extend_from_slice(bytes);
    }
    out
}

/// [`encode_forward`] の逆。版が違う・壊れているときは `None`（受け取らない）。
pub fn decode_forward(bytes: &[u8]) -> Option<Forward> {
    let nul = bytes.iter().position(|&b| b == 0)?;
    let head: Value = serde_json::from_slice(&bytes[..nul]).ok()?;
    if head.get("version")?.as_u64()? != FORWARD_VERSION {
        return None;
    }
    let mut files = Vec::new();
    for f in head.get("files")?.as_array()? {
        let path = PathBuf::from(f.get("path")?.as_str()?);
        if path.as_os_str().is_empty() {
            return None;
        }
        let number = |key: &str| f.get(key).and_then(Value::as_u64).filter(|&n| n >= 1);
        let encoding = match f.get("encoding") {
            Some(Value::String(name)) => Some(parse_new_file_encoding(name)?),
            _ => None,
        };
        files.push(FileSpec {
            path,
            line: number("line"),
            column: number("column"),
            encoding,
            read_only: f.get("read_only").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    let stdin = head
        .get("stdin")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| bytes[nul + 1..].to_vec());
    Some(Forward { files, stdin })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn cwd() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\work")
        } else {
            PathBuf::from("/work")
        }
    }

    fn abs(name: &str) -> PathBuf {
        cwd().join(name)
    }

    fn parse_ok(list: &[&str]) -> Launch {
        parse(&args(list), &cwd()).unwrap()
    }

    #[test]
    fn plain_files_are_resolved_against_cwd() {
        let l = parse_ok(&["a.txt", "sub/b.txt"]);
        assert_eq!(l.files.len(), 2);
        assert_eq!(l.files[0], FileSpec::new(abs("a.txt")));
        assert_eq!(l.files[1].path, abs("sub/b.txt"));
        assert!(!l.stdin && l.window == WindowChoice::Default && !l.no_session);
        assert!(l.has_payload() && l.may_forward());
        assert!(!parse_ok(&[]).has_payload());
    }

    #[test]
    fn absolute_paths_are_kept() {
        let full = if cfg!(windows) {
            r"D:\x\y.txt"
        } else {
            "/x/y.txt"
        };
        assert_eq!(parse_ok(&[full]).files[0].path, PathBuf::from(full));
    }

    #[test]
    fn line_and_column_apply_to_following_files_and_can_be_overridden() {
        let l = parse_ok(&["a", "-n", "5", "b", "-c3", "c", "--line=9", "d"]);
        let got: Vec<_> = l.files.iter().map(|f| (f.line, f.column)).collect();
        assert_eq!(
            got,
            vec![
                (None, None),
                (Some(5), None),
                (Some(5), Some(3)),
                (Some(9), Some(3))
            ]
        );
        // 結合形と +行。
        assert_eq!(parse_ok(&["-n10", "a"]).files[0].line, Some(10));
        assert_eq!(parse_ok(&["+10", "a"]).files[0].line, Some(10));
        assert_eq!(parse_ok(&["--column", "4", "a"]).files[0].column, Some(4));
        // 後ろに書いた +行 は前のファイルに影響しない。
        let l = parse_ok(&["a", "+7", "b"]);
        assert_eq!((l.files[0].line, l.files[1].line), (None, Some(7)));
    }

    #[test]
    fn plus_followed_by_non_digits_is_a_file_name() {
        let l = parse_ok(&["+abc", "+"]);
        assert_eq!(l.files[0].path, abs("+abc"));
        assert_eq!(l.files[1].path, abs("+"));
    }

    #[test]
    fn number_errors() {
        for bad in [
            &["-n", "0"][..],
            &["-n", "-3"],
            &["-n", "x"],
            &["-n10x"],
            &["-c", "0"],
            &["+0"],
            &["--line=abc"],
            &["-n", "99999999999999999999999"],
            &["-g", "a.txt:0"],
            &["-g", "a.txt:1:0"],
        ] {
            assert!(
                matches!(parse(&args(bad), &cwd()), Err(CliError::BadNumber { .. })),
                "{bad:?}"
            );
        }
        assert_eq!(
            parse(&args(&["-n"]), &cwd()),
            Err(CliError::MissingValue("-n".into()))
        );
        assert_eq!(
            parse(&args(&["--goto"]), &cwd()),
            Err(CliError::MissingValue("--goto".into()))
        );
    }

    #[test]
    fn goto_splits_trailing_numbers_and_keeps_drive_letters() {
        assert_eq!(
            split_goto(r"C:\a.txt:10:5"),
            (r"C:\a.txt", Some("10"), Some("5"))
        );
        assert_eq!(split_goto(r"C:\a.txt:10"), (r"C:\a.txt", Some("10"), None));
        assert_eq!(split_goto(r"C:\a.txt"), (r"C:\a.txt", None, None));
        assert_eq!(split_goto("a.txt:3"), ("a.txt", Some("3"), None));
        assert_eq!(split_goto("a.txt:x"), ("a.txt:x", None, None));
        // ファイル名が空になる分け方はしない。
        assert_eq!(split_goto(":5"), (":5", None, None));

        let full = if cfg!(windows) {
            r"C:\src\main.rs"
        } else {
            "/src/main.rs"
        };
        let l = parse_ok(&["-n", "1", "-g", &format!("{full}:42:7"), "other"]);
        assert_eq!(l.files[0].path, PathBuf::from(full));
        assert_eq!((l.files[0].line, l.files[0].column), (Some(42), Some(7)));
        // -g はそのファイルだけ。
        assert_eq!(l.files[1].line, Some(1));
        // 相対パスと行だけ。
        let l = parse_ok(&["--goto", "x.rs:3"]);
        assert_eq!(
            (l.files[0].path.clone(), l.files[0].line, l.files[0].column),
            (abs("x.rs"), Some(3), None)
        );
        // 数字の指定がなければ、ただのファイル。
        let l = parse_ok(&["-g", "x.rs"]);
        assert_eq!((l.files[0].line, l.files[0].column), (None, None));
        assert!(matches!(
            parse(&args(&["-g", ""]), &cwd()),
            Err(CliError::BadGoto(_))
        ));
    }

    #[test]
    fn readonly_and_encoding_apply_to_following_files() {
        let l = parse_ok(&[
            "a",
            "-r",
            "b",
            "-e",
            "shift_jis",
            "c",
            "-esjis",
            "d",
            "-eutf-8-bom",
            "e",
        ]);
        let got: Vec<_> = l.files.iter().map(|f| (f.read_only, f.encoding)).collect();
        assert_eq!(
            got,
            vec![
                (false, None),
                (true, None),
                (true, Some((Encoding::Cp932, false))),
                (true, Some((Encoding::Cp932, false))),
                (true, Some((Encoding::Utf8, true))),
            ]
        );
        assert_eq!(
            parse(&args(&["-e", "latin1", "a"]), &cwd()),
            Err(CliError::BadEncoding("latin1".into()))
        );
        assert!(parse_ok(&["--readonly", "a"]).files[0].read_only);
        assert_eq!(
            parse_ok(&["--encoding=utf-16le", "a"]).files[0].encoding,
            Some((Encoding::Utf16Le, true))
        );
    }

    #[test]
    fn stdin_window_and_session_flags() {
        let l = parse_ok(&["-"]);
        assert!(l.stdin && l.files.is_empty() && l.has_payload());
        assert_eq!(parse_ok(&["-w"]).window, WindowChoice::New);
        assert_eq!(parse_ok(&["--new-window", "a"]).window, WindowChoice::New);
        assert!(!parse_ok(&["-w"]).may_forward());
        assert_eq!(parse_ok(&["--reuse-window"]).window, WindowChoice::Reuse);
        assert!(parse_ok(&["--no-session"]).no_session);
    }

    #[test]
    fn double_dash_makes_the_rest_file_names() {
        let l = parse_ok(&["-n", "2", "--", "-x", "-", "--help", "a"]);
        let got: Vec<_> = l.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(got, vec![abs("-x"), abs("-"), abs("--help"), abs("a")]);
        assert!(!l.stdin && l.info.is_none());
        assert!(l.files.iter().all(|f| f.line == Some(2)));
    }

    #[test]
    fn help_and_version() {
        for a in ["-h", "--help", "/?"] {
            assert_eq!(parse_ok(&[a, "x"]).info, Some(Info::Help), "{a}");
        }
        for a in ["-v", "--version"] {
            assert_eq!(parse_ok(&[a]).info, Some(Info::Version), "{a}");
        }
        assert!(!parse_ok(&["-h"]).may_forward());
        assert!(usage().contains("--new-window"));
        assert!(version_text().starts_with("Linote "));
    }

    #[test]
    fn unknown_options_are_errors_not_file_names() {
        for bad in [
            "-x",
            "--frobnicate",
            "-5",
            "--readonly=1",
            "-rw",
            "-hx",
            "--",
        ] {
            let r = parse(&args(&[bad, "a"]), &cwd());
            if bad == "--" {
                assert!(r.is_ok());
                continue;
            }
            assert!(
                matches!(r, Err(CliError::UnknownOption(ref o)) if o == bad),
                "{bad}: {r:?}"
            );
        }
        assert!(CliError::UnknownOption("-x".into())
            .message()
            .starts_with("不明なオプション: -x"));
    }

    #[test]
    fn internal_options() {
        let l = parse_ok(&["--adopt", "h.json", "--pos", "10,-20"]);
        assert_eq!(l.adopt, Some(abs("h.json")));
        assert_eq!(l.pos, Some((10, -20)));
        assert!(!l.may_forward());
        assert!(parse_ok(&["--self-test"]).self_test);
        assert!(!parse_ok(&["--self-test"]).may_forward());
        // 位置が読めなくても起動はできる。
        assert_eq!(parse_ok(&["--pos", "zzz"]).pos, None);
    }

    #[test]
    fn forward_round_trips_including_binary_stdin() {
        let files = vec![
            FileSpec {
                path: abs("a.txt"),
                line: Some(12),
                column: Some(3),
                encoding: Some((Encoding::Cp932, false)),
                read_only: true,
            },
            FileSpec::new(abs("日本語.txt")),
        ];
        let stdin = vec![0u8, 1, 2, 0xFF, 0, b'x'];
        let bytes = encode_forward(&files, Some(&stdin));
        let back = decode_forward(&bytes).unwrap();
        assert_eq!(back.files, files);
        assert_eq!(back.stdin, Some(stdin));
        let bytes = encode_forward(&files, None);
        assert_eq!(decode_forward(&bytes).unwrap().stdin, None);
        // 標準入力が空でも「あった」ことは残る。
        assert_eq!(
            decode_forward(&encode_forward(&[], Some(&[])))
                .unwrap()
                .stdin,
            Some(Vec::new())
        );
    }

    #[test]
    fn forward_rejects_bad_or_future_data() {
        assert!(decode_forward(b"").is_none());
        assert!(decode_forward(b"{}").is_none());
        assert!(decode_forward(b"not json\0").is_none());
        assert!(decode_forward(br#"{"version":2,"files":[],"stdin":false}"#).is_none());
        let mut ok = br#"{"version":1,"files":[],"stdin":false}"#.to_vec();
        assert!(decode_forward(&ok).is_none(), "NUL がない");
        ok.push(0);
        assert_eq!(decode_forward(&ok), Some(Forward::default()));
        // 不正な文字コード名・空のパスは受け取らない。
        let bad = br#"{"version":1,"files":[{"path":"a","encoding":"zzz"}],"stdin":false}"#;
        let mut bad = bad.to_vec();
        bad.push(0);
        assert!(decode_forward(&bad).is_none());
        let empty = br#"{"version":1,"files":[{"path":""}],"stdin":false}"#;
        let mut empty = empty.to_vec();
        empty.push(0);
        assert!(decode_forward(&empty).is_none());
        // 行・桁の 0 は無視（なし）として読む。
        let zero = br#"{"version":1,"files":[{"path":"a","line":0,"column":0}],"stdin":false}"#;
        let mut zero = zero.to_vec();
        zero.push(0);
        let f = decode_forward(&zero).unwrap();
        assert_eq!((f.files[0].line, f.files[0].column), (None, None));
    }
}
