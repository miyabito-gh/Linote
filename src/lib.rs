//! Linote core library. GUI-independent modules live here so they can be
//! tested headless and reused by the future MCP entry point.

/// Raw FFI to the statically linked Scintilla/Lexilla (see build.rs).
#[cfg(windows)]
pub mod sci_sys {
    extern "C" {
        pub fn Scintilla_RegisterClasses(h_instance: *mut core::ffi::c_void) -> i32;
        pub fn Scintilla_ReleaseResources() -> i32;
        /// Returns an `ILexer5*` for a bundled lexer name, or null.
        pub fn linote_create_lexer(name: *const core::ffi::c_char) -> *mut core::ffi::c_void;
    }
}
