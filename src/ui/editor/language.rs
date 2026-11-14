//! 拡張子から言語（Lexilla の字句解析器、キーワード、スタイル番号と配色の対応）を選ぶ。
//!
//! 対象言語は `third_party/VENDORED.md` の表。Java は `cpp` に Java のキーワードを渡す。
//! TOML は Lexilla 5.3.0 に専用の字句解析器がないので `props` で代用する。

use std::path::Path;

use crate::ui::theme::Token;

/// 構文着色をする本文の上限（バイト）。これを超える文書は着色しない
/// （巨大ファイルは別の表示部品で扱うが、その手前の大きさでも着色の負荷を避ける）。
pub const HIGHLIGHT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// 構文着色の言語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    PlainText,
    C,
    Cpp,
    Java,
    Rust,
    Markdown,
    Json,
    Yaml,
    Ini,
    Toml,
    Html,
    Xml,
    Python,
    PowerShell,
    Batch,
    Sql,
    Log,
}

impl Language {
    pub const ALL: [Language; 17] = [
        Language::PlainText,
        Language::C,
        Language::Cpp,
        Language::Java,
        Language::Rust,
        Language::Markdown,
        Language::Json,
        Language::Yaml,
        Language::Ini,
        Language::Toml,
        Language::Html,
        Language::Xml,
        Language::Python,
        Language::PowerShell,
        Language::Batch,
        Language::Sql,
        Language::Log,
    ];

    /// パスの拡張子（大文字小文字を区別しない）から選ぶ。不明ならプレーンテキスト。
    pub fn from_path(path: &Path) -> Language {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        Self::from_extension(&ext)
    }

    /// 拡張子（`.` なし、小文字）から選ぶ。
    pub fn from_extension(ext: &str) -> Language {
        match ext {
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "inl" | "ipp" => {
                Language::Cpp
            }
            "java" => Language::Java,
            "rs" => Language::Rust,
            "md" | "markdown" | "mkd" | "mdown" => Language::Markdown,
            "json" | "jsonc" | "json5" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "ini" | "cfg" | "conf" | "properties" | "inf" | "reg" | "editorconfig" => Language::Ini,
            "toml" => Language::Toml,
            "html" | "htm" | "xhtml" => Language::Html,
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "xaml" | "csproj" | "vcxproj" | "props"
            | "targets" | "plist" | "config" | "manifest" | "resx" => Language::Xml,
            "py" | "pyw" | "pyi" => Language::Python,
            "ps1" | "psm1" | "psd1" => Language::PowerShell,
            "bat" | "cmd" => Language::Batch,
            "sql" => Language::Sql,
            "log" => Language::Log,
            _ => Language::PlainText,
        }
    }

    /// ステータスバーなどに出す名前。
    pub const fn label(self) -> &'static str {
        match self {
            Language::PlainText => "テキスト",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Java => "Java",
            Language::Rust => "Rust",
            Language::Markdown => "Markdown",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Ini => "INI",
            Language::Toml => "TOML",
            Language::Html => "HTML",
            Language::Xml => "XML",
            Language::Python => "Python",
            Language::PowerShell => "PowerShell",
            Language::Batch => "バッチ",
            Language::Sql => "SQL",
            Language::Log => "ログ",
        }
    }

    /// Lexilla の字句解析器の名前（`linote_create_lexer` に渡す）。
    /// プレーンテキストは `None`（字句解析器を設定しない）。
    pub const fn lexer_name(self) -> Option<&'static str> {
        Some(match self {
            Language::PlainText => return None,
            Language::C | Language::Cpp | Language::Java => "cpp",
            Language::Rust => "rust",
            Language::Markdown => "markdown",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Ini | Language::Toml => "props",
            Language::Html => "hypertext",
            Language::Xml => "xml",
            Language::Python => "python",
            Language::PowerShell => "powershell",
            Language::Batch => "batch",
            Language::Sql => "sql",
            Language::Log => "errorlist",
        })
    }

    /// 字句解析器に渡すキーワード（`SCI_SETKEYWORDS` の番号と、空白区切りの一覧）。
    pub fn keywords(self) -> &'static [(usize, &'static str)] {
        match self {
            Language::C => &[(0, C_KEYWORDS), (1, C_TYPES)],
            Language::Cpp => &[(0, CPP_KEYWORDS), (1, C_TYPES)],
            Language::Java => &[(0, JAVA_KEYWORDS), (1, JAVA_TYPES)],
            Language::Rust => &[(0, RUST_KEYWORDS), (1, RUST_TYPES)],
            Language::Python => &[(0, PYTHON_KEYWORDS), (1, PYTHON_BUILTINS)],
            Language::Sql => &[(0, SQL_KEYWORDS), (1, SQL_TYPES)],
            Language::PowerShell => &[(0, PS_KEYWORDS), (1, PS_CMDLETS)],
            Language::Batch => &[(0, BATCH_KEYWORDS)],
            Language::Json => &[(0, "true false null")],
            Language::Yaml => &[(0, "true false yes no on off null")],
            _ => &[],
        }
    }

    /// 字句解析器のプロパティ（`SCI_SETPROPERTY`）。
    pub fn properties(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // 条件コンパイルで無効な部分を薄く表示する機能は、スタイル番号が 64 以降に
            // ずれて配色の対応が複雑になるので切る。
            Language::C | Language::Cpp | Language::Java => &[
                ("lexer.cpp.track.preprocessor", "0"),
                ("lexer.cpp.update.preprocessor", "0"),
                ("lexer.cpp.escape.sequence", "1"),
            ],
            Language::Json => &[("lexer.json.allow.comments", "1")],
            Language::Html | Language::Xml => &[("fold", "0")],
            _ => &[],
        }
    }

    /// 字句解析器のスタイル番号と、意味の分類の対応。ここにない番号は本文の既定色。
    pub fn styles(self) -> &'static [(u8, Token)] {
        use Token::*;
        match self {
            Language::PlainText => &[],
            Language::C | Language::Cpp | Language::Java => &[
                (1, Comment),
                (2, Comment),
                (3, Comment),
                (15, Comment),
                (17, Comment),
                (18, Comment),
                (23, Comment),
                (24, Comment),
                (26, Comment),
                (4, Number),
                (25, Number),
                (5, Keyword),
                (16, Type),
                (19, Type),
                (6, String),
                (7, String),
                (13, String),
                (20, String),
                (21, String),
                (22, String),
                (27, Number),
                (12, Error),
                (9, Preprocessor),
                (10, Operator),
                (14, Code),
                (8, Number),
            ],
            Language::Rust => &[
                (1, Comment),
                (2, Comment),
                (3, Comment),
                (4, Comment),
                (5, Number),
                (6, Keyword),
                (7, Type),
                (8, Type),
                (9, Type),
                (10, Type),
                (11, Type),
                (12, Type),
                (13, String),
                (14, String),
                (15, String),
                (21, String),
                (22, String),
                (23, String),
                (16, Operator),
                (18, Attribute),
                (19, Preprocessor),
                (20, Error),
            ],
            Language::Markdown => &[
                (2, Emphasis),
                (3, Emphasis),
                (4, Emphasis),
                (5, Emphasis),
                (6, Heading),
                (7, Heading),
                (8, Heading),
                (9, Heading),
                (10, Heading),
                (11, Heading),
                (12, Operator),
                (13, Keyword),
                (14, Keyword),
                (15, Comment),
                (16, Comment),
                (17, Operator),
                (18, Link),
                (19, Code),
                (20, Code),
                (21, Code),
            ],
            Language::Json => &[
                (1, Number),
                (2, String),
                (3, Error),
                (4, Attribute),
                (5, String),
                (6, Comment),
                (7, Comment),
                (8, Operator),
                (9, Link),
                (10, Link),
                (11, Keyword),
                (12, Keyword),
                (13, Error),
            ],
            Language::Yaml => &[
                (1, Comment),
                (2, Attribute),
                (3, Keyword),
                (4, Number),
                (5, Preprocessor),
                (6, Tag),
                (7, String),
                (8, Error),
                (9, Operator),
            ],
            Language::Ini | Language::Toml => &[
                (1, Comment),
                (2, Tag),
                (3, Operator),
                (4, Preprocessor),
                (5, Attribute),
            ],
            Language::Html | Language::Xml => &[
                (1, Tag),
                (2, Tag),
                (11, Tag),
                (3, Attribute),
                (4, Attribute),
                (5, Number),
                (6, String),
                (7, String),
                (19, String),
                (9, Comment),
                (20, Comment),
                (29, Comment),
                (10, Keyword),
                (12, Preprocessor),
                (13, Preprocessor),
                (18, Preprocessor),
                (17, Code),
                (21, Preprocessor),
                (22, Preprocessor),
                (23, Preprocessor),
                (24, String),
                (25, String),
                (26, Error),
                (27, Preprocessor),
                (28, Keyword),
                (30, Comment),
                (31, Preprocessor),
            ],
            Language::Python => &[
                (1, Comment),
                (12, Comment),
                (2, Number),
                (3, String),
                (4, String),
                (6, String),
                (7, String),
                (16, String),
                (17, String),
                (18, String),
                (19, String),
                (5, Keyword),
                (8, Type),
                (9, Type),
                (14, Type),
                (10, Operator),
                (13, Error),
                (15, Preprocessor),
                (20, Attribute),
            ],
            Language::PowerShell => &[
                (1, Comment),
                (13, Comment),
                (16, Comment),
                (2, String),
                (3, String),
                (14, String),
                (15, String),
                (4, Number),
                (5, Attribute),
                (6, Operator),
                (8, Keyword),
                (9, Type),
                (10, Type),
                (11, Type),
            ],
            Language::Batch => &[
                (1, Comment),
                (2, Keyword),
                (3, Tag),
                (4, Operator),
                (5, Type),
                (6, Attribute),
                (7, Operator),
            ],
            Language::Sql => &[
                (1, Comment),
                (2, Comment),
                (3, Comment),
                (13, Comment),
                (15, Comment),
                (17, Comment),
                (18, Comment),
                (4, Number),
                (5, Keyword),
                (6, String),
                (7, String),
                (10, Operator),
                (16, Type),
                (23, Attribute),
            ],
            Language::Log => &[
                (1, Link),
                (2, Link),
                (3, Link),
                (4, Keyword),
                (5, Link),
                (6, Link),
                (7, Link),
                (8, Link),
                (9, Link),
                (10, Number),
                (11, String),
                (12, Error),
                (13, Keyword),
                (14, Link),
                (15, Link),
                (16, Link),
                (17, Link),
                (18, Link),
                (19, Link),
                (20, Link),
                (21, Attribute),
                (22, Link),
                (23, Operator),
                (24, Operator),
                (25, Comment),
                (26, Link),
            ],
        }
    }

    /// 本文の大きさを考慮した実際の言語。大きすぎればプレーンテキスト。
    pub fn effective(self, len: u64) -> Language {
        if len > HIGHLIGHT_MAX_BYTES {
            Language::PlainText
        } else {
            self
        }
    }
}

const C_KEYWORDS: &str = "auto break case const continue default do else enum extern for goto if \
inline register restrict return sizeof static struct switch typedef union volatile while \
_Alignas _Alignof _Atomic _Bool _Complex _Generic _Imaginary _Noreturn _Static_assert \
_Thread_local alignas alignof bool false true nullptr static_assert thread_local typeof";

const CPP_KEYWORDS: &str = "alignas alignof and and_eq asm auto bitand bitor break case catch \
class compl concept const consteval constexpr constinit const_cast continue co_await co_return \
co_yield decltype default delete do dynamic_cast else enum explicit export extern false final \
for friend goto if import inline module mutable namespace new noexcept not not_eq nullptr \
operator or or_eq override private protected public register reinterpret_cast requires return \
sizeof static static_assert static_cast struct switch template this thread_local throw true try \
typedef typeid typename union using virtual volatile while xor xor_eq";

const C_TYPES: &str = "bool char char8_t char16_t char32_t double float int long short signed \
unsigned void wchar_t size_t ssize_t ptrdiff_t intptr_t uintptr_t int8_t int16_t int32_t int64_t \
uint8_t uint16_t uint32_t uint64_t FILE std string vector map unique_ptr shared_ptr";

const JAVA_KEYWORDS: &str = "abstract assert break case catch class const continue default do \
else enum exports extends false final finally for goto if implements import instanceof interface \
module native new non-sealed null open opens package permits private protected provides public \
record requires return sealed static strictfp super switch synchronized this throw throws \
transient transitive true try uses var volatile while with yield";

const JAVA_TYPES: &str = "boolean byte char double float int long short void String Object \
Integer Long Double Float Boolean Character Byte Short List Map Set";

const RUST_KEYWORDS: &str = "as async await break const continue crate dyn else enum extern \
false fn for if impl in let loop match mod move mut pub ref return self Self static struct super \
trait true type union unsafe use where while abstract become box do final macro override priv \
try typeof unsized virtual yield gen";

const RUST_TYPES: &str = "bool char str i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 \
f64 String Vec Option Some None Result Ok Err Box Rc Arc";

const PYTHON_KEYWORDS: &str = "False None True and as assert async await break case class \
continue def del elif else except finally for from global if import in is lambda match nonlocal \
not or pass raise return try while with yield";

const PYTHON_BUILTINS: &str = "abs all any bool bytes dict enumerate float int isinstance len \
list map max min object open print range repr self set sorted str sum super tuple type zip";

const SQL_KEYWORDS: &str = "add all alter and any as asc begin between by case check column \
commit constraint create cross database default delete desc distinct drop else end exists \
foreign from full group having if in index inner insert into is join key left like limit not \
null offset on or order outer primary references right rollback select set table then top \
transaction truncate union unique update values view when where with";

const SQL_TYPES: &str = "bigint binary bit blob boolean char date datetime decimal double float \
int integer money nchar numeric nvarchar real smallint text time timestamp tinyint varbinary \
varchar";

const PS_KEYWORDS: &str = "begin break catch class continue data define do dynamicparam else \
elseif end enum exit filter finally for foreach from function hidden if in param process \
return static switch throw trap try until using var while";

const PS_CMDLETS: &str = "add-content clear-host copy-item foreach-object get-childitem \
get-command get-content get-help get-item get-location get-process get-service import-module \
invoke-command invoke-webrequest move-item new-item new-object out-file read-host remove-item \
rename-item select-object select-string set-content set-item set-location sort-object \
start-process stop-process test-path where-object write-error write-host write-output \
write-verbose write-warning";

const BATCH_KEYWORDS: &str = "assoc break call cd chdir cls color copy date defined del dir \
do echo else endlocal equ erase errorlevel exist exit for ftype geq goto gtr if in leq lss md \
mkdir move neq not nul path pause popd prompt pushd rd rem ren rename rmdir set setlocal shift \
start time title type ver verify vol";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_select_languages() {
        let cases = [
            ("a.rs", Language::Rust),
            ("A.JAVA", Language::Java),
            ("x.h", Language::C),
            ("x.hpp", Language::Cpp),
            ("README.md", Language::Markdown),
            ("Cargo.toml", Language::Toml),
            ("a.yml", Language::Yaml),
            ("s.ps1", Language::PowerShell),
            ("run.CMD", Language::Batch),
            ("index.html", Language::Html),
            ("a.svg", Language::Xml),
            ("app.log", Language::Log),
            ("notes.txt", Language::PlainText),
            ("Makefile", Language::PlainText),
        ];
        for (path, lang) in cases {
            assert_eq!(Language::from_path(Path::new(path)), lang, "{path}");
        }
    }

    #[test]
    fn every_language_maps_to_a_bundled_lexer() {
        // `third_party/VENDORED.md` の字句解析器の名前。
        let bundled = [
            "cpp",
            "rust",
            "markdown",
            "json",
            "yaml",
            "props",
            "hypertext",
            "xml",
            "python",
            "powershell",
            "batch",
            "sql",
            "errorlist",
        ];
        for lang in Language::ALL {
            match lang.lexer_name() {
                None => assert_eq!(lang, Language::PlainText),
                Some(name) => assert!(bundled.contains(&name), "{lang:?} -> {name}"),
            }
        }
        assert_eq!(Language::Java.lexer_name(), Some("cpp"));
        assert!(Language::Java.keywords()[0].1.contains("synchronized"));
    }

    #[test]
    fn style_tables_have_no_duplicates() {
        for lang in Language::ALL {
            let styles = lang.styles();
            for (i, (s, _)) in styles.iter().enumerate() {
                assert!(*s < 64, "{lang:?} style {s}");
                assert!(
                    !styles[i + 1..].iter().any(|(o, _)| o == s),
                    "{lang:?} duplicates style {s}"
                );
            }
        }
    }

    #[test]
    fn large_documents_are_not_highlighted() {
        assert_eq!(Language::Rust.effective(1024), Language::Rust);
        assert_eq!(
            Language::Rust.effective(HIGHLIGHT_MAX_BYTES + 1),
            Language::PlainText
        );
    }
}
