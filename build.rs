//! Compiles the bundled Scintilla and Lexilla sources (third_party/) into a
//! static library linked into the single Linote executable.
use std::path::Path;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }
    let sci = Path::new("third_party/scintilla");
    let lex = Path::new("third_party/lexilla");
    println!("cargo:rerun-if-changed=third_party");
    println!("cargo:rerun-if-changed=res");
    embed_manifest();

    let mut b = cc::Build::new();
    b.cpp(true)
        .std("c++17")
        .warnings(false)
        .define("NDEBUG", None)
        .define("STATIC_BUILD", None)
        .define("SCI_EMPTYCATALOGUE", None)
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .include(sci.join("include"))
        .include(sci.join("src"))
        .include(sci.join("win32"))
        .include(lex.join("include"))
        .include(lex.join("lexlib"));
    // GCC/Clang 系: 同梱の Scintilla/Lexilla は <cstdint> を include せず INT32_MAX / uint8_t を使う
    // ファイルがある（CellBuffer.cxx など）。古い libstdc++ は他のヘッダー経由で暗黙に取り込んでいたが、
    // GCC 15 系（MSYS2 の mingw64 など）では取り込まれずコンパイルエラーになる。
    // ベンダ同梱コードを書き換えずに済むよう、強制 include で補う（MSVC は対象外）。
    if !b.get_compiler().is_like_msvc() {
        b.flag("-include").flag("cstdint");
    }
    for dir in [
        sci.join("src"),
        sci.join("win32"),
        lex.join("lexlib"),
        lex.join("lexers"),
        lex.join("src"),
    ] {
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "cxx"))
            .collect();
        files.sort();
        b.files(files);
    }
    // Link libstdc++ statically so the EXE has no libstdc++-6.dll dependency.
    b.cpp_link_stdlib(None);
    b.compile("scintilla_lexilla");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        let compiler = b.get_compiler();
        let out = std::process::Command::new(compiler.path())
            .arg("-print-file-name=libstdc++.a")
            .output()
            .expect("query libstdc++.a location");
        let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if let Some(dir) = path.parent() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        println!("cargo:rustc-link-lib=static=stdc++");
    }

    for lib in [
        "user32", "gdi32", "imm32", "ole32", "oleaut32", "uuid", "msimg32", "advapi32", "comctl32",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}

/// アプリケーションマニフェスト（コモンコントロール v6、PerMonitorV2、長いパス）を EXE に埋め込む。
fn embed_manifest() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("linote_res.o");
    let windres = find_windres();
    let status = std::process::Command::new(windres)
        .args([
            "--input",
            "res/linote.rc",
            "--output-format=coff",
            "--include-dir",
            "res",
            "--output",
        ])
        .arg(&out)
        .status()
        .expect("run windres");
    assert!(status.success(), "windres failed");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// windres の実行ファイルを決める。優先順位:
/// 1. 環境変数 `WINDRES`（明示指定）
/// 2. 接頭辞付き `x86_64-w64-mingw32-windres`（Debian/Ubuntu のクロスツールチェーン。普段の経路）
/// 3. 接頭辞なし `windres`（MSYS2 の mingw64 など、接頭辞付きの別名を持たない環境）
fn find_windres() -> String {
    if let Ok(w) = std::env::var("WINDRES") {
        if !w.is_empty() {
            return w;
        }
    }
    for name in ["x86_64-w64-mingw32-windres", "windres"] {
        let ok = std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return name.into();
        }
    }
    // どれも見つからない場合は従来の名前で実行し、「run windres」の失敗として表に出す。
    "x86_64-w64-mingw32-windres".into()
}
