//! 読み込み → 書き出しの往復（タスク A）。
//!
//! - `tests/fixtures/` の全ファイルが、読んで保存するとバイト単位で一致する。
//! - 乱数で作った本文・バイト列でも往復が成り立つ（依存を増やさない簡易な乱数）。
//! - 100 MiB 級の計測は `--ignored` で実行する（`large_file_timing`）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use linote::core::{DocStamp, DocumentId, TaskContext, Version};
use linote::file::{
    check_external_change, read_text, save_text, DiskExpectation, ExternalChange, LoadedText,
    ReadOptions, SaveOutcome, SaveRequest,
};
use linote::text::{
    self, Confidence, DetectReason, Encoding, Eol, EolStats, OnUnmappable, TextFormat,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "linote-it-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ctx() -> TaskContext {
    TaskContext::detached()
}

fn doc() -> DocStamp {
    DocStamp::new(DocumentId::from_raw(1), Version::from_raw(1))
}

fn load(path: &Path) -> LoadedText {
    read_text(path, &ReadOptions::default(), &ctx()).unwrap()
}

fn save(path: &Path, text: &str, format: TextFormat, expect: DiskExpectation) -> SaveOutcome {
    save_text(
        &SaveRequest {
            path,
            text,
            format,
            doc: doc(),
            expect,
            on_unmappable: OnUnmappable::Fail,
        },
        &ctx(),
    )
    .unwrap()
}

struct Fixture {
    name: &'static str,
    bytes: &'static [u8],
    encoding: Encoding,
    bom: bool,
    eol: Eol,
    stats: (u64, u64, u64), // (crlf, lf, cr)
}

macro_rules! fixture {
    ($name:literal, $encoding:expr, $bom:expr, $eol:expr, $stats:expr) => {
        Fixture {
            name: $name,
            bytes: include_bytes!(concat!("fixtures/", $name)),
            encoding: $encoding,
            bom: $bom,
            eol: $eol,
            stats: $stats,
        }
    };
}

const SAMPLE: &str = "日本語のテキスト、半角ｶﾀｶﾅ、機種依存文字①Ⅱ㈱、ASCII abc 123\n二行目：「ありがとう」\n三行目 end\n";

fn fixtures() -> Vec<Fixture> {
    use Encoding::*;
    vec![
        fixture!("utf8-nobom.txt", Utf8, false, Eol::Lf, (0, 3, 0)),
        fixture!("utf8-bom.txt", Utf8, true, Eol::Lf, (0, 3, 0)),
        fixture!("utf16le-bom.txt", Utf16Le, true, Eol::Lf, (0, 3, 0)),
        fixture!("utf16be-bom.txt", Utf16Be, true, Eol::Lf, (0, 3, 0)),
        fixture!("cp932.txt", Cp932, false, Eol::Lf, (0, 3, 0)),
        fixture!("eol-crlf.txt", Utf8, false, Eol::CrLf, (3, 0, 0)),
        fixture!("eol-lf.txt", Utf8, false, Eol::Lf, (0, 3, 0)),
        fixture!("eol-cr.txt", Utf8, false, Eol::Cr, (0, 0, 3)),
        fixture!("eol-mixed.txt", Utf8, false, Eol::CrLf, (2, 1, 1)),
        fixture!("no-trailing-newline.txt", Utf8, false, Eol::Lf, (0, 2, 0)),
        // 空のファイル: 文字コードは既定の UTF-8、改行は既定（CRLF）。
        fixture!("empty.txt", Utf8, false, Eol::CrLf, (0, 0, 0)),
        fixture!("unmappable-cp932.txt", Utf8, false, Eol::Lf, (0, 1, 0)),
    ]
}

#[test]
fn every_fixture_is_listed() {
    // fixtures/ に増えたファイルを、往復テストに入れ忘れないようにする。
    let listed: Vec<&str> = fixtures().iter().map(|f| f.name).collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    // Wine から Linux 側のパスが見えない環境では省略する。
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.ends_with(".txt") {
            assert!(listed.contains(&name.as_str()), "未登録の fixture: {name}");
        }
    }
}

#[test]
fn fixtures_round_trip_byte_for_byte() {
    let dir = TempDir::new("fixtures");
    for f in fixtures() {
        let source = dir.path().join(f.name);
        fs::write(&source, f.bytes).unwrap();

        let loaded = load(&source);
        assert_eq!(loaded.format.encoding, f.encoding, "{}", f.name);
        assert_eq!(loaded.format.bom, f.bom, "{}", f.name);
        assert_eq!(loaded.format.eol, f.eol, "{}", f.name);
        assert_eq!(
            (loaded.eol.crlf, loaded.eol.lf, loaded.eol.cr),
            f.stats,
            "{}",
            f.name
        );
        assert!(!loaded.is_lossy(), "{}", f.name);
        assert!(loaded.reencodes_exactly, "{}", f.name);
        assert!(
            loaded.detection.confidence >= Confidence::Medium,
            "{}",
            f.name
        );
        assert_eq!(loaded.stamp.len, f.bytes.len() as u64);
        assert!(!loaded.text.starts_with('\u{FEFF}'), "{}", f.name);

        // メモリ上の変換でも一致する。
        let encoded =
            text::encode(&loaded.text, loaded.format, OnUnmappable::Fail, &ctx()).unwrap();
        assert_eq!(encoded, f.bytes, "{}", f.name);

        // 別名で保存。
        let copy = dir.path().join(format!("copy-{}", f.name));
        match save(&copy, &loaded.text, loaded.format, DiskExpectation::Absent) {
            SaveOutcome::Saved(saved) => assert_eq!(saved.bytes_written, f.bytes.len() as u64),
            other => panic!("{}: {other:?}", f.name),
        }
        assert_eq!(fs::read(&copy).unwrap(), f.bytes, "{}", f.name);

        // 上書き保存（外部変更の確認つき）。
        let expect = DiskExpectation::Unchanged(loaded.stamp);
        let saved = match save(&source, &loaded.text, loaded.format, expect) {
            SaveOutcome::Saved(saved) => saved,
            other => panic!("{}: {other:?}", f.name),
        };
        assert_eq!(fs::read(&source).unwrap(), f.bytes, "{}", f.name);
        assert_eq!(
            check_external_change(&source, Some(saved.disk)).unwrap(),
            ExternalChange::None
        );
    }
    // 一時ファイルが残っていない。
    assert_eq!(dir.entries().len(), fixtures().len() * 2);
}

#[test]
fn fixture_contents_match_the_readme() {
    let ctx = ctx();
    for f in fixtures() {
        let detection = text::detect(f.bytes);
        let decoded = text::decode(f.bytes.to_vec(), detection.encoding, &ctx).unwrap();
        match f.name {
            "utf8-nobom.txt" | "utf8-bom.txt" | "utf16le-bom.txt" | "utf16be-bom.txt"
            | "cp932.txt" => assert_eq!(decoded.text, SAMPLE, "{}", f.name),
            "eol-crlf.txt" => assert_eq!(decoded.text, "one\r\ntwo\r\nthree\r\n"),
            "eol-lf.txt" => assert_eq!(decoded.text, "one\ntwo\nthree\n"),
            "eol-cr.txt" => assert_eq!(decoded.text, "one\rtwo\rthree\r"),
            "eol-mixed.txt" => {
                assert_eq!(decoded.text, "crlf\r\nlf\ncr\rlast\r\n");
                assert!(EolStats::scan(decoded.text.as_bytes()).is_mixed());
            }
            "no-trailing-newline.txt" => assert_eq!(decoded.text, "one\ntwo\nthree"),
            "empty.txt" => {
                assert_eq!(decoded.text, "");
                assert_eq!(detection.reason, DetectReason::Empty);
            }
            "unmappable-cp932.txt" => {
                assert_eq!(decoded.text, "絵文字 😀 と 𠮷野家 と通常の日本語\n")
            }
            other => panic!("期待値のない fixture: {other}"),
        }
    }
}

#[test]
fn unmappable_fixture_warns_before_overwriting() {
    let dir = TempDir::new("unmappable");
    let path = dir.path().join("a.txt");
    let bytes: &[u8] = include_bytes!("fixtures/unmappable-cp932.txt");
    fs::write(&path, bytes).unwrap();
    let loaded = load(&path);

    // CP932 に変えて上書きしようとすると、書く前に止まる。
    let cp932 = TextFormat {
        encoding: Encoding::Cp932,
        bom: false,
        eol: loaded.format.eol,
    };
    let outcome = save(
        &path,
        &loaded.text,
        cp932,
        DiskExpectation::Unchanged(loaded.stamp),
    );
    match outcome {
        SaveOutcome::Unmappable { chars, .. } => {
            let got: Vec<(char, u64, u64)> = chars
                .value
                .iter()
                .map(|u| (u.ch, u.line, u.column))
                .collect();
            assert_eq!(got, [('😀', 0, 4), ('𠮷', 0, 8)]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(dir.entries(), ["a.txt"]);
    assert_eq!(
        check_external_change(&path, Some(loaded.stamp)).unwrap(),
        ExternalChange::None
    );
}

// ---- 乱数による性質テスト --------------------------------------------------

/// xorshift64*。再現できるよう種は固定。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// CP932 でも表せる部品。
const COMMON_PARTS: &[&str] = &[
    "a",
    "Z",
    "0",
    " ",
    "\t",
    "~",
    "\\",
    "\n",
    "\r\n",
    "\r",
    "\n\n",
    "\r\r\n",
    "あ",
    "ん",
    "ア",
    "ｶ",
    "ﾟ",
    "漢",
    "字",
    "語",
    "、",
    "。",
    "「",
    "」",
    "①",
    "Ⅱ",
    "㈱",
    "髙",
    "﨑",
    "～",
    "－",
    "∥",
    "￢",
    "￤",
    "㍻",
    "\u{E000}",
    "\u{E757}",
    "\u{80}",
    "ABC",
    "日本語",
    "x=1;",
];

/// Unicode でしか表せない部品。
const UNICODE_PARTS: &[&str] = &[
    "😀",
    "𠮷",
    "é",
    "ß",
    "¥",
    "‾",
    "−",
    "\u{FEFF}",
    "\u{FFFD}",
    "\u{0}",
    "\u{7F}",
    "한",
    "ไ",
    "\u{10FFFF}",
    "\u{FFFF}",
    "e\u{301}",
    "👨\u{200D}👩\u{200D}👧",
];

fn random_text(rng: &mut Rng, unicode: bool) -> String {
    let len = match rng.below(10) {
        0 => 0,
        1 => 1,
        _ => rng.below(400),
    };
    let mut text = String::new();
    for _ in 0..len {
        if unicode && rng.below(4) == 0 {
            text.push_str(rng.pick(UNICODE_PARTS));
        } else {
            text.push_str(rng.pick(COMMON_PARTS));
        }
    }
    text
}

fn all_formats() -> Vec<TextFormat> {
    let mut formats = Vec::new();
    for encoding in Encoding::ALL {
        for bom in [false, true] {
            if bom && encoding.bom().is_empty() {
                continue;
            }
            formats.push(TextFormat {
                encoding,
                bom,
                eol: Eol::Lf,
            });
        }
    }
    formats
}

#[test]
fn random_texts_survive_encode_then_decode() {
    let ctx = ctx();
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for round in 0..600 {
        for format in all_formats() {
            let unicode = format.encoding != Encoding::Cp932;
            let original = random_text(&mut rng, unicode);
            let bytes = text::encode(&original, format, OnUnmappable::Fail, &ctx)
                .unwrap_or_else(|e| panic!("round {round} {format:?}: {e}"));
            assert!(bytes.starts_with(if format.bom {
                format.encoding.bom()
            } else {
                b""
            }));
            let decoded = text::decode(bytes.clone(), format.encoding, &ctx).unwrap();
            // 本文が BOM と同じ文字で始まる BOM なしの文書だけは、読み直すと BOM と
            // 区別できない（先頭の U+FEFF は BOM として扱う取り決め）。
            if !format.bom && original.starts_with('\u{FEFF}') && unicode {
                assert!(decoded.bom);
                assert_eq!(decoded.text, original['\u{FEFF}'.len_utf8()..]);
            } else {
                assert_eq!(decoded.text, original, "round {round} {format:?}");
                assert_eq!(decoded.bom, format.bom, "round {round} {format:?}");
            }
            assert!(!decoded.is_lossy(), "round {round} {format:?}");
            assert!(decoded.reencodes_exactly, "round {round} {format:?}");
            // もう一度書き出すと同じバイト列。
            let again = text::encode(
                &decoded.text,
                TextFormat {
                    bom: decoded.bom,
                    ..format
                },
                OnUnmappable::Fail,
                &ctx,
            )
            .unwrap();
            assert_eq!(again, bytes, "round {round} {format:?}");
        }
    }
}

#[test]
fn random_files_survive_load_then_save() {
    let dir = TempDir::new("random");
    let ctx = ctx();
    let mut rng = Rng(0xDEAD_BEEF_0BAD_F00D);
    let path = dir.path().join("doc.txt");
    for round in 0..150 {
        for format in all_formats() {
            let unicode = format.encoding != Encoding::Cp932;
            let original = random_text(&mut rng, unicode);
            let bytes = text::encode(&original, format, OnUnmappable::Fail, &ctx).unwrap();
            fs::write(&path, &bytes).unwrap();

            // 文字コードを指定して開く（BOM なし UTF-16 などは自動判定が保証できない）。
            let options = ReadOptions::with_encoding(format.encoding);
            let loaded = read_text(&path, &options, &ctx).unwrap();
            assert!(!loaded.is_lossy(), "round {round} {format:?}");
            assert!(loaded.reencodes_exactly, "round {round} {format:?}");

            // 編集せずに上書き保存 → 同じバイト列。
            let expect = DiskExpectation::Unchanged(loaded.stamp);
            match save(&path, &loaded.text, loaded.format, expect) {
                SaveOutcome::Saved(saved) => {
                    assert_eq!(saved.bytes_written, bytes.len() as u64);
                }
                other => panic!("round {round} {format:?}: {other:?}"),
            }
            assert_eq!(fs::read(&path).unwrap(), bytes, "round {round} {format:?}");

            // BOM があれば自動判定でも同じ結果になる。
            if format.bom {
                let auto = load(&path);
                assert_eq!(auto.format.encoding, format.encoding);
                assert_eq!(auto.text, loaded.text);
            }
        }
    }
    assert_eq!(dir.entries(), ["doc.txt"]);
}

#[test]
fn auto_detected_utf8_and_cp932_texts_round_trip() {
    // BOM なしの UTF-8 と CP932 は、日本語を十分に含めば自動判定で往復する。
    let ctx = ctx();
    let mut rng = Rng(0x0F0F_F0F0_1357_9BDF);
    for round in 0..300 {
        let mut body = String::from("これは日本語の文章です。判定の手がかりになります。\r\n");
        body.push_str(&random_text(&mut rng, false).replace('\u{80}', ""));
        for encoding in [Encoding::Utf8, Encoding::Cp932] {
            let format = TextFormat {
                encoding,
                bom: false,
                eol: Eol::CrLf,
            };
            let bytes = text::encode(&body, format, OnUnmappable::Fail, &ctx).unwrap();
            let detection = text::detect(&bytes);
            assert_eq!(detection.encoding, encoding, "round {round}: {body:?}");
            assert!(!detection.bom);
            let decoded = text::decode(bytes, detection.encoding, &ctx).unwrap();
            assert_eq!(decoded.text, body, "round {round}");
        }
    }
}

#[test]
fn random_bytes_never_panic_and_exactness_is_honest() {
    // どんなバイト列を読んでも落ちない。「一致する」と報告した場合は、書き出すと
    // 本当に元のバイト列に戻る。「不正なバイトなし」の場合は、文字として往復する。
    let ctx = ctx();
    let mut rng = Rng(0xABCD_EF01_2345_6789);
    // CP932 の 2 バイト文字らしい列が多く出るよう、値の偏りを変えた 3 種類。
    let pools: [&[u8]; 3] = [
        &[
            0x00, 0x0A, 0x0D, 0x20, 0x41, 0x5C, 0x7E, 0x7F, 0x80, 0x81, 0x82, 0x87, 0x9F, 0xA0,
            0xA1, 0xDF, 0xE0, 0xED, 0xEE, 0xEF, 0xF0, 0xF9, 0xFA, 0xFC, 0xFD, 0xFE, 0xFF, 0x40,
            0xD8, 0xDC, 0xC2, 0xE3, 0xBF, 0xBB,
        ],
        &[
            0x82, 0xA0, 0x82, 0xA2, 0x41, 0x0A, 0xED, 0x40, 0xFA, 0x40, 0x87, 0x54,
        ],
        &[],
    ];
    for round in 0..3000 {
        let pool = pools[round % pools.len()];
        let len = rng.below(64);
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                if pool.is_empty() {
                    rng.next() as u8
                } else {
                    *rng.pick(pool)
                }
            })
            .collect();
        for encoding in Encoding::ALL {
            let decoded = text::decode(bytes.clone(), encoding, &ctx).unwrap();
            assert_eq!(
                decoded.is_lossy(),
                decoded.text.contains('\u{FFFD}') && decoded.invalid.count > 0,
                "{encoding:?} {bytes:02X?}"
            );
            assert!(decoded
                .invalid
                .first_offsets
                .iter()
                .all(|&o| o < len as u64));
            let format = TextFormat {
                encoding,
                bom: decoded.bom,
                eol: Eol::Lf,
            };
            let reencoded = text::encode(&decoded.text, format, OnUnmappable::Fail, &ctx);
            if decoded.reencodes_exactly {
                assert_eq!(reencoded.unwrap(), bytes, "{encoding:?} {bytes:02X?}");
            } else if !decoded.is_lossy() {
                // CP932 の重複文字: バイト列は変わるが、文字としては同じ。
                assert_eq!(encoding, Encoding::Cp932);
                let again = text::decode(reencoded.unwrap(), encoding, &ctx).unwrap();
                assert_eq!(again.text, decoded.text, "{bytes:02X?}");
                assert!(again.reencodes_exactly);
            }
            // 判定も落ちない。
            let _ = text::detect(&bytes);
        }
    }
}

#[test]
fn every_cp932_double_byte_sequence_is_accounted_for() {
    // 2 バイトの全組み合わせ: 読めるなら、(a) 書き出すと元に戻る、または
    // (b) 重複文字として報告され、書き出しても同じ文字。
    let ctx = ctx();
    let (mut exact, mut duplicates, mut invalid) = (0u32, 0u32, 0u32);
    for lead in 0x81..=0xFCu8 {
        if (0xA0..=0xDF).contains(&lead) {
            continue;
        }
        for trail in 0x40..=0xFCu8 {
            let bytes = vec![lead, trail];
            let decoded = text::decode(bytes.clone(), Encoding::Cp932, &ctx).unwrap();
            if decoded.is_lossy() {
                invalid += 1;
                continue;
            }
            let format = TextFormat {
                encoding: Encoding::Cp932,
                bom: false,
                eol: Eol::Lf,
            };
            let reencoded = text::encode(&decoded.text, format, OnUnmappable::Fail, &ctx)
                .unwrap_or_else(|e| panic!("{bytes:02X?} → {:?}: {e}", decoded.text));
            if decoded.reencodes_exactly {
                assert_eq!(reencoded, bytes);
                exact += 1;
            } else {
                let again = text::decode(reencoded, Encoding::Cp932, &ctx).unwrap();
                assert_eq!(again.text, decoded.text, "{bytes:02X?}");
                duplicates += 1;
            }
        }
    }
    // 件数は encoding_rs の表による（変わったら architecture.md の記述も見直す）。
    println!("cp932: exact={exact} duplicates={duplicates} invalid={invalid}");
    assert!(exact > 9000, "{exact}");
    assert!(duplicates > 300 && duplicates < 500, "{duplicates}");
}

/// 100 MiB 級の読み込み・保存の計測。
/// `LINOTE_LARGE_MIB=100 cargo test --release --test file_roundtrip -- --ignored --nocapture large_file_timing`
#[test]
#[ignore = "計測用（100 MiB 級のファイルを作る）"]
fn large_file_timing() {
    use std::time::Instant;
    let dir = TempDir::new("large");
    // 大きさ（MiB）を環境変数で指定したときだけ動く（`--ignored` の一括実行で
    // 巻き込まれないように）。
    let Some(mib) = std::env::var("LINOTE_LARGE_MIB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let line = "2024-06-01 12:34:56 INFO 日本語のログ行です。ｶﾀｶﾅ ①㈱ ASCII text 0123456789\r\n";
    let body = line.repeat(mib * (1 << 20) / line.len());
    for format in all_formats() {
        let path = dir
            .path()
            .join(format!("{}-{}.txt", format.encoding.name(), format.bom));
        let t = Instant::now();
        let first = save(&path, &body, format, DiskExpectation::Absent);
        let save_ms = t.elapsed().as_millis();
        assert!(matches!(first, SaveOutcome::Saved(_)));
        let original = fs::read(&path).unwrap();

        let t = Instant::now();
        let loaded = load(&path);
        let load_ms = t.elapsed().as_millis();
        assert_eq!(loaded.format.encoding, format.encoding);
        assert!(loaded.reencodes_exactly);
        assert_eq!(loaded.text.len(), body.len());

        let t = Instant::now();
        let expect = DiskExpectation::Unchanged(loaded.stamp);
        let again = save(&path, &loaded.text, loaded.format, expect);
        let resave_ms = t.elapsed().as_millis();
        assert!(matches!(again, SaveOutcome::Saved(_)));
        assert!(fs::read(&path).unwrap() == original);
        println!(
            "{:<18} {:>4} MiB on disk: save {save_ms} ms, load {load_ms} ms, re-save {resave_ms} ms",
            format.label(),
            original.len() >> 20
        );
        fs::remove_file(&path).unwrap();
    }
}
