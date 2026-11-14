//! 文字コードの判定。
//!
//! 順序: BOM → UTF-16 らしさ（NUL を含む場合だけ） → UTF-8 として妥当か → CP932。
//! どれにも当てはまらなければ UTF-8 とみなし、信頼度を [`Confidence::Low`] にする
//! （不正なバイトは [`decode`](super::decode) が数えて報告する。黙って壊さない）。
//!
//! NUL を含む場合に UTF-16 を先に調べるのは、BOM なし UTF-16 の ASCII 文書
//! （`a\0b\0`）が UTF-8 としても妥当なため。

use super::Encoding;

/// 判定の信頼度。大小比較できる（`Low < Medium < High < Certain`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// 当て推量。利用者に文字コードの指定を促すとよい。
    Low,
    /// 経験則による推定（BOM なし UTF-16、CP932）。
    Medium,
    /// ほぼ確実（妥当な UTF-8）。
    High,
    /// 確定（BOM、または利用者の指定）。
    Certain,
}

/// 判定の根拠。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectReason {
    /// BOM があった。
    Bom,
    /// 空のファイル（既定の UTF-8）。
    Empty,
    /// ASCII だけ（UTF-8 とみなす）。
    Ascii,
    /// 多バイト文字を含み、全体が UTF-8 として妥当。
    ValidUtf8,
    /// BOM はないが、NUL の分布と内容が UTF-16 に合う。
    Utf16Pattern,
    /// UTF-8 としては不正で、CP932 としては妥当。
    ValidCp932,
    /// 不正なバイトがあるが、大半は UTF-8 として妥当。
    MostlyUtf8,
    /// どれにも当てはまらない（UTF-8 とみなした）。
    Fallback,
    /// 利用者が指定した。
    Specified,
}

impl DetectReason {
    /// 画面に出せる短い説明。
    pub const fn describe(self) -> &'static str {
        match self {
            DetectReason::Bom => "BOM がある",
            DetectReason::Empty => "空のファイル",
            DetectReason::Ascii => "ASCII のみ",
            DetectReason::ValidUtf8 => "UTF-8 として妥当",
            DetectReason::Utf16Pattern => "NUL の分布が UTF-16 に合う",
            DetectReason::ValidCp932 => "UTF-8 としては不正で、CP932 として妥当",
            DetectReason::MostlyUtf8 => "不正なバイトを含むが、大半は UTF-8",
            DetectReason::Fallback => "判定できない（UTF-8 とみなした）",
            DetectReason::Specified => "利用者の指定",
        }
    }
}

/// 文字コードの判定結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Detection {
    pub encoding: Encoding,
    /// 先頭に BOM があるか。
    pub bom: bool,
    pub confidence: Confidence,
    pub reason: DetectReason,
}

impl Detection {
    /// 利用者が文字コードを指定した場合の結果。BOM は、先頭がその文字コードの BOM
    /// であれば「あり」とする。
    pub fn specified(bytes: &[u8], encoding: Encoding) -> Self {
        Detection {
            encoding,
            bom: has_bom(bytes, encoding),
            confidence: Confidence::Certain,
            reason: DetectReason::Specified,
        }
    }
}

/// `bytes` の先頭が `encoding` の BOM か。
pub fn has_bom(bytes: &[u8], encoding: Encoding) -> bool {
    let bom = encoding.bom();
    !bom.is_empty() && bytes.starts_with(bom)
}

/// 経験則（UTF-16 らしさ、CP932 の妥当性）を調べる範囲。先頭からこのバイト数まで。
/// UTF-8 の妥当性は全体を調べる（速いので）。
const SAMPLE_LEN: usize = 1 << 20;

/// 文字コードを判定する。`bytes` はファイルの全体（先頭からの一部でも動くが、
/// UTF-8 の妥当性は渡された範囲だけで判断する）。
pub fn detect(bytes: &[u8]) -> Detection {
    let make = |encoding, bom, confidence, reason| Detection {
        encoding,
        bom,
        confidence,
        reason,
    };
    if bytes.is_empty() {
        return make(Encoding::Utf8, false, Confidence::High, DetectReason::Empty);
    }
    for encoding in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
        if has_bom(bytes, encoding) {
            return make(encoding, true, Confidence::Certain, DetectReason::Bom);
        }
    }

    let sample = &bytes[..bytes.len().min(SAMPLE_LEN)];
    if sample.contains(&0) {
        if let Some((encoding, confidence)) = guess_utf16(bytes.len(), sample) {
            // 根拠が弱い（ASCII が少ない）ときは、UTF-8 として妥当ならそちらを優先する。
            if confidence >= Confidence::Medium || std::str::from_utf8(bytes).is_err() {
                return make(encoding, false, confidence, DetectReason::Utf16Pattern);
            }
        }
    }

    if bytes.is_ascii() {
        return make(Encoding::Utf8, false, Confidence::High, DetectReason::Ascii);
    }
    if std::str::from_utf8(bytes).is_ok() {
        return make(
            Encoding::Utf8,
            false,
            Confidence::High,
            DetectReason::ValidUtf8,
        );
    }

    // UTF-8 としては不正。CP932 の文書を UTF-8 として読むと妥当な多バイト列はほとんど
    // 現れないので、妥当な多バイト文字が不正な箇所よりずっと多ければ「壊れた UTF-8」。
    let (utf8_multibyte, utf8_errors) = utf8_quality(sample);
    let mostly_utf8 = utf8_multibyte > utf8_errors.saturating_mul(8);
    if !mostly_utf8 && cp932_is_clean(sample, sample.len() == bytes.len()) {
        return make(
            Encoding::Cp932,
            false,
            Confidence::Medium,
            DetectReason::ValidCp932,
        );
    }
    let reason = if mostly_utf8 {
        DetectReason::MostlyUtf8
    } else {
        DetectReason::Fallback
    };
    make(Encoding::Utf8, false, Confidence::Low, reason)
}

/// （妥当な多バイト文字の数, 不正な箇所の数）。
fn utf8_quality(sample: &[u8]) -> (u64, u64) {
    let mut multibyte = 0u64;
    let mut errors = 0u64;
    for chunk in sample.utf8_chunks() {
        // 先頭バイト（0xC0 以上）を数えれば多バイト文字の数になる。
        multibyte += chunk
            .valid()
            .as_bytes()
            .iter()
            .filter(|&&b| b >= 0xC0)
            .count() as u64;
        if !chunk.invalid().is_empty() {
            errors += 1;
        }
    }
    (multibyte, errors)
}

/// `sample` が CP932 として不正な列を含まないか。`complete` はファイルの末尾まで
/// 含んでいるか（途中で切った標本なら、末尾の 2 バイト文字の片割れは不正としない）。
fn cp932_is_clean(sample: &[u8], complete: bool) -> bool {
    let mut decoder = encoding_rs::SHIFT_JIS.new_decoder_without_bom_handling();
    let mut scratch = [0u8; 8192];
    let mut read = 0;
    loop {
        let (result, n, _written) =
            decoder.decode_to_utf8_without_replacement(&sample[read..], &mut scratch, complete);
        read += n;
        match result {
            encoding_rs::DecoderResult::InputEmpty => return true,
            encoding_rs::DecoderResult::OutputFull => {}
            encoding_rs::DecoderResult::Malformed(_, _) => return false,
        }
    }
}

/// BOM のない UTF-16 かどうかを推定する。NUL を含む場合だけ呼ぶ。
///
/// 両方のバイト順で 16 ビット単位に読み、次を満たす方を選ぶ:
/// 対にならないサロゲートがない、制御文字（タブ・改行以外。U+0000 を含む）がない、
/// ASCII の印字可能文字・タブ・改行の数が相手より多い。
fn guess_utf16(total_len: usize, sample: &[u8]) -> Option<(Encoding, Confidence)> {
    if !total_len.is_multiple_of(2) {
        return None;
    }
    let sample = &sample[..sample.len() & !1];
    if sample.is_empty() {
        return None;
    }
    let le = utf16_plausibility(sample, u16::from_le_bytes);
    let be = utf16_plausibility(sample, u16::from_be_bytes);
    let units = (sample.len() / 2) as u64;
    let (encoding, ascii) = match (le, be) {
        (Some(l), Some(b)) if l > b => (Encoding::Utf16Le, l),
        (Some(l), Some(b)) if b > l => (Encoding::Utf16Be, b),
        (Some(l), None) if l > 0 => (Encoding::Utf16Le, l),
        (None, Some(b)) if b > 0 => (Encoding::Utf16Be, b),
        _ => return None,
    };
    // 半分以上が ASCII なら、偶然とは考えにくい。
    let confidence = if ascii * 2 >= units {
        Confidence::Medium
    } else {
        Confidence::Low
    };
    Some((encoding, confidence))
}

/// UTF-16 として読めるなら ASCII らしい単位の数、読めなければ `None`。
fn utf16_plausibility(sample: &[u8], read: fn([u8; 2]) -> u16) -> Option<u64> {
    let mut ascii = 0u64;
    let mut pending_high = false;
    for pair in sample.chunks_exact(2) {
        let unit = read([pair[0], pair[1]]);
        let is_low = (0xDC00..=0xDFFF).contains(&unit);
        if pending_high != is_low {
            return None; // 対にならないサロゲート
        }
        pending_high = (0xD800..=0xDBFF).contains(&unit);
        match unit {
            0x09 | 0x0A | 0x0D | 0x20..=0x7E => ascii += 1,
            0x00..=0x1F | 0x7F => return None,
            0xFFFE | 0xFFFF => return None, // 非文字
            _ => {}
        }
    }
    // 標本の末尾が上位サロゲートで切れているのは許す。
    Some(ascii)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str, le: bool) -> Vec<u8> {
        s.encode_utf16()
            .flat_map(|u| if le { u.to_le_bytes() } else { u.to_be_bytes() })
            .collect()
    }

    #[test]
    fn bom_wins() {
        let d = detect(b"\xEF\xBB\xBFabc");
        assert_eq!(
            (d.encoding, d.bom, d.confidence, d.reason),
            (Encoding::Utf8, true, Confidence::Certain, DetectReason::Bom)
        );
        let d = detect(b"\xFF\xFEa\0");
        assert_eq!((d.encoding, d.bom), (Encoding::Utf16Le, true));
        let d = detect(b"\xFE\xFF\0a");
        assert_eq!((d.encoding, d.bom), (Encoding::Utf16Be, true));
        // BOM だけのファイル。
        assert_eq!(detect(b"\xFF\xFE").encoding, Encoding::Utf16Le);
        assert_eq!(detect(b"\xEF\xBB\xBF").encoding, Encoding::Utf8);
    }

    #[test]
    fn empty_and_ascii_are_utf8() {
        let d = detect(b"");
        assert_eq!(
            (d.encoding, d.bom, d.reason),
            (Encoding::Utf8, false, DetectReason::Empty)
        );
        let d = detect(b"hello\r\nworld");
        assert_eq!(
            (d.encoding, d.reason),
            (Encoding::Utf8, DetectReason::Ascii)
        );
    }

    #[test]
    fn utf8_and_cp932() {
        let text = "日本語のテキスト、半角ｶﾀｶﾅ、①Ⅱ㈱ abc\n";
        let d = detect(text.as_bytes());
        assert_eq!(
            (d.encoding, d.confidence, d.reason),
            (Encoding::Utf8, Confidence::High, DetectReason::ValidUtf8)
        );
        let (sjis, _, bad) = encoding_rs::SHIFT_JIS.encode(text);
        assert!(!bad);
        let d = detect(&sjis);
        assert_eq!(
            (d.encoding, d.bom, d.confidence, d.reason),
            (
                Encoding::Cp932,
                false,
                Confidence::Medium,
                DetectReason::ValidCp932
            )
        );
    }

    #[test]
    fn utf16_without_bom() {
        let text = "hello, world\r\n日本語 mixed with ascii\r\n";
        for le in [true, false] {
            let d = detect(&utf16(text, le));
            let want = if le {
                Encoding::Utf16Le
            } else {
                Encoding::Utf16Be
            };
            assert_eq!(
                (d.encoding, d.bom, d.reason),
                (want, false, DetectReason::Utf16Pattern)
            );
            assert_eq!(d.confidence, Confidence::Medium);
        }
        // サロゲートペアを含んでもよい。
        let d = detect(&utf16("a😀b\n", true));
        assert_eq!(d.encoding, Encoding::Utf16Le);
    }

    #[test]
    fn nul_in_plain_text_is_not_utf16() {
        // NUL を含むが UTF-16 には見えない → UTF-8 として妥当ならそのまま。
        let d = detect(b"abc\0def\n");
        assert_eq!(d.encoding, Encoding::Utf8);
        // 奇数長は UTF-16 にしない。
        let d = detect(b"a\0b\0c");
        assert_eq!(d.encoding, Encoding::Utf8);
    }

    #[test]
    fn broken_utf8_stays_utf8_with_low_confidence() {
        let mut bytes = "日本語の長い文章です。これは UTF-8 で書かれています。\n"
            .repeat(4)
            .into_bytes();
        bytes.push(0xFF);
        bytes.extend_from_slice("続き\n".as_bytes());
        let d = detect(&bytes);
        assert_eq!(
            (d.encoding, d.confidence, d.reason),
            (Encoding::Utf8, Confidence::Low, DetectReason::MostlyUtf8)
        );
    }

    #[test]
    fn garbage_falls_back() {
        let d = detect(&[0xFF, 0xFF, 0xFD, 0xA0, 0x80, 0xFE]);
        assert_eq!(
            (d.encoding, d.confidence, d.reason),
            (Encoding::Utf8, Confidence::Low, DetectReason::Fallback)
        );
    }

    #[test]
    fn specified_reports_bom() {
        let d = Detection::specified(b"\xFF\xFEa\0", Encoding::Utf16Le);
        assert!(d.bom);
        assert_eq!(d.confidence, Confidence::Certain);
        let d = Detection::specified(b"\xFF\xFEa\0", Encoding::Cp932);
        assert!(!d.bom);
        assert!(Confidence::Low < Confidence::Medium && Confidence::High < Confidence::Certain);
    }
}
