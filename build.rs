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
