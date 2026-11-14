//! 利用者のテキストファイルの読み込みと保存。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::atomic::{write_atomic_checked, AtomicOptions, ReplaceMode};
use super::FileStamp;
use crate::core::{DocStamp, Error, Limits, Outcome, Result, TaskContext};
use crate::text::{
    self, Detection, Encoding, Eol, EolStats, InvalidBytes, OnUnmappable, TextFormat, Unmappable,
};

/// 読み込みの指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    /// 文字コード。`None` なら自動判定。利用者が指定して開き直すときに `Some`。
    pub encoding: Option<Encoding>,
    /// 改行が一つもないファイルの [`TextFormat::eol`]（新しく入力する改行）。
    pub default_eol: Eol,
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            encoding: None,
            default_eol: TextFormat::DEFAULT.eol,
        }
    }
}

impl ReadOptions {
    /// `encoding` を指定して開き直すための指定。
    pub fn with_encoding(encoding: Encoding) -> Self {
        ReadOptions {
            encoding: Some(encoding),
            ..Self::default()
        }
    }
}

/// 読み込んだテキストファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedText {
    /// 本文（UTF-8、BOM なし、改行は元のまま）。
    pub text: String,
    /// 保存時に保持する形式（文字コード、BOM、最も多い改行）。
    pub format: TextFormat,
    /// 読み始める前のディスク上の状態。読んでいる間に変更された場合は、次の照合で
    /// 食い違って外部変更として扱われる。
    pub stamp: FileStamp,
    /// 文字コードの判定の根拠と信頼度（指定して開いた場合は `Specified`）。
    pub detection: Detection,
    /// 改行の内訳。[`EolStats::is_mixed`] なら混在。
    pub eol: EolStats,
    /// 不正なバイト列（U+FFFD に置き換えた箇所）。
    pub invalid: InvalidBytes,
    /// 編集せずに保存すると、元のバイト列と完全に一致するか。
    pub reencodes_exactly: bool,
}

impl LoadedText {
    /// 不正なバイトを置き換えたか。真なら、利用者に知らせ、上書き保存の前に確認する
    /// （保存すると不正だった箇所は U+FFFD として書かれ、元には戻せない）。
    pub fn is_lossy(&self) -> bool {
        self.invalid.count > 0
    }
}

/// ファイルの全体を読む。読み始める前の [`FileStamp`] も返す。
///
/// `ctx.limits.max_bytes` を超える大きさなら読まずに [`Error::LimitExceeded`]
/// （通常編集と巨大モードの振り分けに使える）。進捗はバイト数。
pub fn read_bytes(path: &Path, ctx: &TaskContext) -> Result<(Vec<u8>, FileStamp)> {
    const SEGMENT: u64 = 1 << 20;
    let read_error = |e| Error::io("読み込み", path, e);
    ctx.cancel.check()?;
    let mut file = File::open(path).map_err(read_error)?;
    let meta = file.metadata().map_err(read_error)?;
    if meta.is_dir() {
        return Err(read_error(super::failure::is_directory_error()));
    }
    let stamp = FileStamp::from_metadata(&meta);
    let too_large = |limit| Error::LimitExceeded {
        what: format!("ファイルの大きさ: {}", path.display()),
        limit,
    };
    if let Some(limit) = ctx.limits.max_bytes {
        if stamp.len > limit {
            return Err(too_large(limit));
        }
    }
    let expected = usize::try_from(stamp.len).map_err(|_| too_large(usize::MAX as u64))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .map_err(|_| read_error(std::io::ErrorKind::OutOfMemory.into()))?;
    loop {
        ctx.cancel.check()?;
        let n = (&mut file)
            .take(SEGMENT)
            .read_to_end(&mut bytes)
            .map_err(read_error)?;
        if n == 0 {
            break;
        }
        // 読んでいる間に伸びた場合も上限を守る。
        if let Some(limit) = ctx.limits.max_bytes {
            if bytes.len() as u64 > limit {
                return Err(too_large(limit));
            }
        }
        ctx.report(bytes.len() as u64, Some(stamp.len.max(bytes.len() as u64)));
    }
    Ok((bytes, stamp))
}

/// テキストファイルを読んで、編集用の UTF-8 と形式にする。
///
/// 文字コードは自動判定（[`text::detect`]）するか、`options.encoding` の指定に従う。
/// 不正なバイトは U+FFFD に置き換えて [`LoadedText::invalid`] で報告する
/// （エラーにはしない。利用者が文字コードを選び直せるようにするため）。
pub fn read_text(path: &Path, options: &ReadOptions, ctx: &TaskContext) -> Result<LoadedText> {
    let (bytes, stamp) = read_bytes(path, ctx)?;
    decode_file_bytes(bytes, stamp, options, ctx)
}

/// [`read_text`] の、読み込みを除いた部分（すでにバイト列を持っている場合用）。
pub fn decode_file_bytes(
    bytes: Vec<u8>,
    stamp: FileStamp,
    options: &ReadOptions,
    ctx: &TaskContext,
) -> Result<LoadedText> {
    let detection = match options.encoding {
        Some(encoding) => Detection::specified(&bytes, encoding),
        None => text::detect(&bytes),
    };
    let decoded = text::decode(bytes, detection.encoding, ctx)?;
    ctx.cancel.check()?;
    let eol = EolStats::scan(decoded.text.as_bytes());
    Ok(LoadedText {
        format: TextFormat {
            encoding: detection.encoding,
            bom: decoded.bom,
            eol: eol.dominant().unwrap_or(options.default_eol),
        },
        text: decoded.text,
        stamp,
        detection,
        eol,
        invalid: decoded.invalid,
        reencodes_exactly: decoded.reencodes_exactly,
    })
}

/// 保存の直前に確かめる、ディスク上の状態の前提。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskExpectation {
    /// 確かめない。「名前を付けて保存」で利用者が上書きを承知した場合や、外部変更を
    /// 知ったうえで上書きを選んだ場合。
    Any,
    /// 存在しないはず（新しい名前での保存）。存在したら [`Error::ExternalChange`]。
    Absent,
    /// 最後に開いた・保存したときの状態のままのはず。食い違えば（消えていても）
    /// [`Error::ExternalChange`]。
    Unchanged(FileStamp),
}

impl DiskExpectation {
    /// 文書が控えている状態から前提を作る（控えがなければ「存在しないはず」）。
    pub fn from_stamp(stamp: Option<FileStamp>) -> Self {
        match stamp {
            Some(stamp) => DiskExpectation::Unchanged(stamp),
            None => DiskExpectation::Absent,
        }
    }

    /// いまのディスク上の状態が前提どおりか確かめる。
    pub fn verify(&self, path: &Path) -> Result<()> {
        let matches = match self {
            DiskExpectation::Any => return Ok(()),
            DiskExpectation::Absent => FileStamp::read(path)?.is_none(),
            DiskExpectation::Unchanged(stamp) => FileStamp::read(path)? == Some(*stamp),
        };
        if matches {
            Ok(())
        } else {
            Err(Error::ExternalChange {
                path: path.to_path_buf(),
            })
        }
    }
}

/// 保存の依頼。バックグラウンドで実行できるよう、本文の写しと文書の印を受け取る。
#[derive(Debug, Clone, Copy)]
pub struct SaveRequest<'a> {
    pub path: &'a Path,
    /// 本文（UTF-8）。改行はそのまま書く。
    pub text: &'a str,
    /// 書き出す形式（文字コードと BOM。`eol` は使わない）。
    pub format: TextFormat,
    /// 保存する内容の「文書 ID＋版」。結果にそのまま載せて返す。
    pub doc: DocStamp,
    pub expect: DiskExpectation,
    /// 変換できない文字の扱い。まず [`OnUnmappable::Fail`] で呼び、
    /// [`SaveOutcome::Unmappable`] が返ったら利用者に確認し、承知したら
    /// [`OnUnmappable::Replace`]（`?` に置換）で呼び直す。
    pub on_unmappable: OnUnmappable,
}

/// 保存できた。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Saved {
    pub doc: DocStamp,
    /// 保存後のディスク上の状態（`DocumentState::record_saved` に渡す）。
    pub disk: FileStamp,
    /// 書いたバイト数（BOM を含む）。
    pub bytes_written: u64,
}

/// [`save_text`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved(Saved),
    /// 変換できない文字があるので**何も書かなかった**（元ファイルはそのまま）。
    /// 一覧は [`MAX_UNMAPPABLE_REPORT`] 件（または `ctx.limits.max_items`）で打ち切る。
    Unmappable {
        doc: DocStamp,
        chars: Outcome<Vec<Unmappable>>,
    },
}

/// 変換できない文字の一覧の、既定の上限。
pub const MAX_UNMAPPABLE_REPORT: u64 = 1000;

/// テキストを `format` の文字コード・BOM で、一時ファイル経由で保存する。
///
/// 1. `expect` を確かめる（食い違えば [`Error::ExternalChange`]。何も書かない）。
/// 2. 同じディレクトリの一時ファイルへ変換しながら書き、確定させる。
///    変換できない文字があれば一時ファイルを捨てて [`SaveOutcome::Unmappable`]。
/// 3. 置換の直前にもう一度 `expect` を確かめてから、既存の属性を引き継いで置換する
///    （[`ReplaceMode::PreserveMetadata`]）。
///
/// どの失敗でも元ファイルは無傷で、一時ファイルは残らない。失敗の原因は
/// [`classify`](super::classify) で分類できる。進捗は本文（UTF-8）のバイト数。
pub fn save_text(request: &SaveRequest<'_>, ctx: &TaskContext) -> Result<SaveOutcome> {
    let SaveRequest {
        path,
        text,
        format,
        doc,
        expect,
        on_unmappable,
    } = *request;
    expect.verify(path)?;

    let mut bytes_written = 0;
    let options = AtomicOptions {
        mode: ReplaceMode::PreserveMetadata,
    };
    let result = write_atomic_checked(
        path,
        options,
        ctx,
        |out| {
            bytes_written = text::encode_to(text, format, on_unmappable, out, ctx)?;
            Ok(())
        },
        || expect.verify(path),
    );
    match result {
        Ok(disk) => Ok(SaveOutcome::Saved(Saved {
            doc,
            disk,
            bytes_written,
        })),
        Err(Error::Encoding { .. }) if on_unmappable == OnUnmappable::Fail => {
            let limits = Limits {
                max_items: Some(ctx.limits.max_items.unwrap_or(MAX_UNMAPPABLE_REPORT)),
                max_bytes: None,
            };
            let chars =
                text::find_unmappable(text, format.encoding, &ctx.clone().with_limits(limits))?;
            Ok(SaveOutcome::Unmappable { doc, chars })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Completion, DocumentId, Version};
    use crate::file::testutil::TempDir;
    use crate::file::{check_external_change, ExternalChange};
    use std::fs;

    fn ctx() -> TaskContext {
        TaskContext::detached()
    }

    fn doc() -> DocStamp {
        DocStamp::new(DocumentId::from_raw(1), Version::from_raw(3))
    }

    fn request<'a>(path: &'a Path, text: &'a str, format: TextFormat) -> SaveRequest<'a> {
        SaveRequest {
            path,
            text,
            format,
            doc: doc(),
            expect: DiskExpectation::Any,
            on_unmappable: OnUnmappable::Fail,
        }
    }

    fn saved(outcome: SaveOutcome) -> Saved {
        match outcome {
            SaveOutcome::Saved(s) => s,
            other => panic!("{other:?}"),
        }
    }

    const CP932: TextFormat = TextFormat {
        encoding: Encoding::Cp932,
        bom: false,
        eol: Eol::CrLf,
    };

    #[test]
    fn load_detects_format_and_eol() {
        let dir = TempDir::new("io-load");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\r\nthree\n").unwrap();
        let loaded = read_text(&path, &ReadOptions::default(), &ctx()).unwrap();
        assert_eq!(loaded.text, "one\r\ntwo\r\nthree\n");
        assert_eq!(
            loaded.format,
            TextFormat {
                encoding: Encoding::Utf8,
                bom: true,
                eol: Eol::CrLf
            }
        );
        assert!(loaded.eol.is_mixed());
        assert!(!loaded.is_lossy() && loaded.reencodes_exactly);
        assert_eq!(loaded.stamp.len, 19);
        assert_eq!(Some(loaded.stamp), FileStamp::read(&path).unwrap());

        // 改行のないファイルは指定の既定を使う。
        fs::write(&path, b"no newline").unwrap();
        let options = ReadOptions {
            encoding: None,
            default_eol: Eol::Lf,
        };
        assert_eq!(
            read_text(&path, &options, &ctx()).unwrap().format.eol,
            Eol::Lf
        );
        assert_eq!(
            read_text(&path, &ReadOptions::default(), &ctx())
                .unwrap()
                .format,
            TextFormat::DEFAULT
        );
    }

    #[test]
    fn reopen_with_a_specified_encoding() {
        let dir = TempDir::new("io-reopen");
        let path = dir.path().join("a.txt");
        // CP932 の「日本語」。自動判定でも CP932 になるが、UTF-8 を指定すると壊れて見える。
        fs::write(&path, [0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]).unwrap();
        let auto = read_text(&path, &ReadOptions::default(), &ctx()).unwrap();
        assert_eq!(auto.text, "日本語");
        assert_eq!(auto.format.encoding, Encoding::Cp932);

        let forced = read_text(&path, &ReadOptions::with_encoding(Encoding::Utf8), &ctx()).unwrap();
        assert_eq!(forced.format.encoding, Encoding::Utf8);
        assert!(forced.is_lossy());
        assert!(!forced.reencodes_exactly);
        assert_eq!(
            forced.detection.reason,
            crate::text::DetectReason::Specified
        );
        assert!(forced.text.contains('\u{FFFD}'));
    }

    #[test]
    fn missing_file_and_size_limit() {
        let dir = TempDir::new("io-missing");
        let path = dir.path().join("a.txt");
        let err = read_text(&path, &ReadOptions::default(), &ctx()).unwrap_err();
        assert_eq!(
            crate::file::classify(&err),
            Some(crate::file::IoFailure::NotFound)
        );

        fs::write(&path, vec![b'x'; 100]).unwrap();
        let limited = ctx().with_limits(Limits {
            max_items: None,
            max_bytes: Some(99),
        });
        let err = read_bytes(&path, &limited).unwrap_err();
        assert_eq!(err.code(), "limit_exceeded");
        let limited = ctx().with_limits(Limits {
            max_items: None,
            max_bytes: Some(100),
        });
        assert_eq!(read_bytes(&path, &limited).unwrap().0.len(), 100);

        let err = read_bytes(dir.path(), &ctx()).unwrap_err();
        assert_eq!(err.code(), "io");
    }

    #[test]
    fn save_writes_the_requested_format() {
        let dir = TempDir::new("io-save");
        let path = dir.path().join("a.txt");
        let format = TextFormat {
            encoding: Encoding::Utf16Le,
            bom: true,
            eol: Eol::CrLf,
        };
        let out = saved(save_text(&request(&path, "aあ\r\n", format), &ctx()).unwrap());
        assert_eq!(out.doc, doc());
        assert_eq!(out.bytes_written, 10);
        assert_eq!(
            fs::read(&path).unwrap(),
            [0xFF, 0xFE, 0x61, 0x00, 0x42, 0x30, 0x0D, 0x00, 0x0A, 0x00]
        );
        assert_eq!(Some(out.disk), FileStamp::read(&path).unwrap());
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn unmappable_chars_are_reported_before_anything_is_written() {
        let dir = TempDir::new("io-unmappable");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let text = "絵文字 😀 と 𠮷野家\n";
        let outcome = save_text(&request(&path, text, CP932), &ctx()).unwrap();
        match outcome {
            SaveOutcome::Unmappable { doc: d, chars } => {
                assert_eq!(d, doc());
                assert!(chars.completion.is_complete());
                let got: Vec<char> = chars.value.iter().map(|u| u.ch).collect();
                assert_eq!(got, ['😀', '𠮷']);
                assert_eq!((chars.value[0].line, chars.value[0].column), (0, 4));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["a.txt"]);

        // 利用者が承知した場合だけ置換して書く。
        let mut req = request(&path, text, CP932);
        req.on_unmappable = OnUnmappable::Replace;
        saved(save_text(&req, &ctx()).unwrap());
        let reread =
            read_text(&path, &ReadOptions::with_encoding(Encoding::Cp932), &ctx()).unwrap();
        assert_eq!(reread.text, "絵文字 ? と ?野家\n");

        // 一覧は上限で打ち切る。
        let many = "😀".repeat(50);
        let limited = ctx().with_limits(Limits::items(5));
        match save_text(&request(&path, &many, CP932), &limited).unwrap() {
            SaveOutcome::Unmappable { chars, .. } => {
                assert_eq!(chars.value.len(), 5);
                assert_eq!(chars.completion, Completion::ItemLimit);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn external_change_blocks_the_save() {
        let dir = TempDir::new("io-external");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let loaded = read_text(&path, &ReadOptions::default(), &ctx()).unwrap();
        assert_eq!(
            check_external_change(&path, Some(loaded.stamp)).unwrap(),
            ExternalChange::None
        );

        // 別のプログラムが書き換えた（長さが変わる）。
        fs::write(&path, b"changed elsewhere").unwrap();
        assert!(matches!(
            check_external_change(&path, Some(loaded.stamp)).unwrap(),
            ExternalChange::Modified(_)
        ));
        let mut req = request(&path, "mine", TextFormat::DEFAULT);
        req.expect = DiskExpectation::Unchanged(loaded.stamp);
        let err = save_text(&req, &ctx()).unwrap_err();
        assert_eq!(err.code(), "external_change");
        assert_eq!(fs::read(&path).unwrap(), b"changed elsewhere");
        assert_eq!(dir.entries(), ["a.txt"]);

        // 消された場合も上書きしない（作り直さない）。
        fs::remove_file(&path).unwrap();
        assert_eq!(
            check_external_change(&path, Some(loaded.stamp)).unwrap(),
            ExternalChange::Removed
        );
        let err = save_text(&req, &ctx()).unwrap_err();
        assert_eq!(err.code(), "external_change");
        assert!(dir.entries().is_empty());

        // 利用者が上書きを選べば書ける。その後は新しい控えで続けられる。
        req.expect = DiskExpectation::Any;
        let first = saved(save_text(&req, &ctx()).unwrap());
        req.expect = DiskExpectation::Unchanged(first.disk);
        req.text = "mine again";
        let second = saved(save_text(&req, &ctx()).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"mine again");
        assert_eq!(
            check_external_change(&path, Some(second.disk)).unwrap(),
            ExternalChange::None
        );
    }

    #[test]
    fn new_file_must_not_appear_meanwhile() {
        let dir = TempDir::new("io-absent");
        let path = dir.path().join("new.txt");
        let mut req = request(&path, "content", TextFormat::DEFAULT);
        req.expect = DiskExpectation::from_stamp(None);
        assert_eq!(req.expect, DiskExpectation::Absent);
        saved(save_text(&req, &ctx()).unwrap());
        // 2 回目は「存在しないはず」が崩れている。
        req.text = "other";
        let err = save_text(&req, &ctx()).unwrap_err();
        assert_eq!(err.code(), "external_change");
        assert_eq!(fs::read(&path).unwrap(), b"content");
        assert!(matches!(
            check_external_change(&path, None).unwrap(),
            ExternalChange::Created(_)
        ));
    }

    #[test]
    fn change_during_the_write_is_caught_before_replacing() {
        // 一時ファイルを書いている間に外部で変更された場合、置換の直前の確認で止まる。
        let dir = TempDir::new("io-race");
        let path = dir.path().join("a.txt");
        fs::write(&path, b"original").unwrap();
        let stamp = FileStamp::read(&path).unwrap().unwrap();
        let expect = DiskExpectation::Unchanged(stamp);
        let err = write_atomic_checked(
            &path,
            AtomicOptions::default(),
            &ctx(),
            |out| {
                out.write_all(b"mine").unwrap();
                fs::write(&path, b"changed while saving").unwrap();
                Ok(())
            },
            || expect.verify(&path),
        )
        .unwrap_err();
        assert_eq!(err.code(), "external_change");
        assert_eq!(fs::read(&path).unwrap(), b"changed while saving");
        assert_eq!(dir.entries(), ["a.txt"]);
    }

    #[test]
    fn failed_save_keeps_the_original() {
        let dir = TempDir::new("io-fail");
        // 保存先のフォルダーがない。
        let path = dir.path().join("missing").join("a.txt");
        let err = save_text(&request(&path, "x", TextFormat::DEFAULT), &ctx()).unwrap_err();
        assert_eq!(
            crate::file::classify(&err),
            Some(crate::file::IoFailure::NotFound)
        );

        // 読み取り専用。
        let path = dir.path().join("ro.txt");
        fs::write(&path, b"original").unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&path, perms).unwrap();
        let err = save_text(&request(&path, "x", TextFormat::DEFAULT), &ctx()).unwrap_err();
        assert_eq!(
            crate::file::classify(&err),
            Some(crate::file::IoFailure::ReadOnly)
        );
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(dir.entries(), ["ro.txt"]);
        let mut perms = fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&path, perms).unwrap();
    }
}
