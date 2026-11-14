//! **仮のファイル入出力（統合 I1 で `crate::file`／`crate::text` の実装に置き換える）。**
//!
//! タスク B の時点では `file`・`text` の読み書き（タスク A）がまだないので、画面の骨格を
//! 動かすための最小限だけをここに置く:
//!
//! - 読み込み: ファイル全体を読み、UTF-8（BOM の有無）としてだけ解釈する。UTF-8 として
//!   読めなければ [`Error::Encoding`] で断る（内容を壊して開かない）。改行形式は最も多いもの。
//! - 保存: UTF-8（BOM は形式に従う）で、同じディレクトリの一時ファイルに書いて同期し、
//!   置換する（失敗したら一時ファイルを消し、元のファイルに触れない）。
//! - UI スレッドで同期して行う（本来はワーカーで行う。I1 で置き換える）。
//!
//! 使う共有の型は [`TextFormat`]、[`FileStamp`]、[`Error`] だけ。

use std::io::Write;
use std::path::Path;

use crate::core::{Error, Result};
use crate::file::FileStamp;
use crate::text::{Encoding, Eol, TextFormat};

/// 仮の読み込みで扱う大きさの上限。これを超えるファイルは巨大ファイルの表示（J・L）の
/// 担当なので、ここでは断る。
pub const STUB_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// 読み込んだ結果。
#[derive(Debug)]
pub struct Loaded {
    pub text: String,
    pub format: TextFormat,
    pub stamp: FileStamp,
}

/// 最も多い改行（なければ CRLF）。同数なら CRLF、LF、CR の順に優先する。
pub fn detect_eol(text: &str) -> Eol {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    if crlf >= lf && crlf >= cr {
        // 改行がない場合（すべて 0）もここに来る。
        Eol::CrLf
    } else if lf >= cr {
        Eol::Lf
    } else {
        Eol::Cr
    }
}

/// バイト列を UTF-8 として解釈する（BOM を除く）。
pub fn decode_utf8(bytes: Vec<u8>) -> Result<(String, TextFormat)> {
    let bom = bytes.starts_with(Encoding::Utf8.bom());
    let body = if bom {
        bytes[Encoding::Utf8.bom().len()..].to_vec()
    } else {
        bytes
    };
    let text = String::from_utf8(body).map_err(|e| Error::Encoding {
        reason: format!(
            "UTF-8 として読めません（{} バイト目）。この版では UTF-8 だけを開けます",
            e.utf8_error().valid_up_to()
        ),
    })?;
    let format = TextFormat {
        encoding: Encoding::Utf8,
        bom,
        eol: detect_eol(&text),
    };
    Ok((text, format))
}

/// ファイルを読む（仮）。
pub fn read(path: &Path) -> Result<Loaded> {
    let meta = std::fs::metadata(path).map_err(|e| Error::io("読み込み", path, e))?;
    if meta.len() > STUB_MAX_BYTES {
        return Err(Error::LimitExceeded {
            what: "開けるファイルの大きさ".into(),
            limit: STUB_MAX_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|e| Error::io("読み込み", path, e))?;
    let (text, format) = decode_utf8(bytes)?;
    Ok(Loaded {
        text,
        format,
        stamp: FileStamp::from_metadata(&meta),
    })
}

/// 本文を UTF-8 で書く（仮）。UTF-8 以外の形式は断る（変換は I1 で `text` を使う）。
pub fn write(path: &Path, text: &str, format: &TextFormat) -> Result<FileStamp> {
    if format.encoding != Encoding::Utf8 {
        return Err(Error::unsupported(format!(
            "{} での保存（この版では UTF-8 だけ）",
            format.encoding.label()
        )));
    }
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.linote-tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp).map_err(|e| Error::io("書き込み", &tmp, e))?;
        if format.bom {
            f.write_all(Encoding::Utf8.bom())
                .map_err(|e| Error::io("書き込み", &tmp, e))?;
        }
        f.write_all(text.as_bytes())
            .map_err(|e| Error::io("書き込み", &tmp, e))?;
        f.sync_all().map_err(|e| Error::io("書き込み", &tmp, e))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| Error::io("置き換え", path, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    let meta = std::fs::metadata(path).map_err(|e| Error::io("確認", path, e))?;
    Ok(FileStamp::from_metadata(&meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_detection_prefers_the_majority() {
        assert_eq!(detect_eol(""), Eol::CrLf);
        assert_eq!(detect_eol("a\nb\nc\r\n"), Eol::Lf);
        assert_eq!(detect_eol("a\r\nb\r\nc\n"), Eol::CrLf);
        assert_eq!(detect_eol("a\rb\rc\n"), Eol::Cr);
        assert_eq!(detect_eol("a\r\nb\n"), Eol::CrLf);
    }

    #[test]
    fn utf8_decoding_handles_the_bom_and_refuses_other_encodings() {
        let (t, f) = decode_utf8(b"\xEF\xBB\xBFabc\n".to_vec()).unwrap();
        assert_eq!(t, "abc\n");
        assert!(f.bom);
        assert_eq!(f.eol, Eol::Lf);
        let (t, f) = decode_utf8("日本語".as_bytes().to_vec()).unwrap();
        assert_eq!(t, "日本語");
        assert!(!f.bom);
        assert!(matches!(
            decode_utf8(vec![0x82, 0xA0]),
            Err(Error::Encoding { .. })
        ));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "linote-stubio-{}-{}",
            std::process::id(),
            crate::core::MemoId::generate()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        let format = TextFormat {
            encoding: Encoding::Utf8,
            bom: true,
            eol: Eol::Lf,
        };
        let stamp = write(&path, "日本語\tタブ\n", &format).unwrap();
        assert_eq!(stamp.len, 3 + "日本語\tタブ\n".len() as u64);
        let loaded = read(&path).unwrap();
        assert_eq!(loaded.text, "日本語\tタブ\n");
        assert_eq!(loaded.format, format);
        assert_eq!(loaded.stamp, stamp);
        // 一時ファイルは残らない。
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let cp932 = TextFormat {
            encoding: Encoding::Cp932,
            ..format
        };
        assert!(write(&path, "x", &cp932).is_err());
        assert_eq!(read(&path).unwrap().text, "日本語\tタブ\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
