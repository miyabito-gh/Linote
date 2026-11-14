//! Scintilla の定数と構造体（`third_party/scintilla/include/Scintilla.h` 5.4.1 から写したもの）。
//! 使うものだけを置く。値を足すときはヘッダーと照らし合わせること。

#![allow(dead_code)]

use std::ffi::{c_char, c_int, c_void};

// --- メッセージ ---
pub const SCI_GETLENGTH: u32 = 2006;
pub const SCI_INDICSETSTYLE: u32 = 2080;
pub const SCI_INDICSETFORE: u32 = 2082;
pub const SCI_INDICSETUNDER: u32 = 2510;
pub const SCI_SETINDICATORCURRENT: u32 = 2500;
pub const SCI_INDICATORFILLRANGE: u32 = 2504;
pub const SCI_INDICATORCLEARRANGE: u32 = 2505;
pub const SCI_INDICATOREND: u32 = 2509;
pub const SCI_INDICATORVALUEAT: u32 = 2507;
pub const SCI_INDICSETALPHA: u32 = 2523;
pub const SCI_INDICSETOUTLINEALPHA: u32 = 2558;
pub const INDIC_STRAIGHTBOX: usize = 8;
pub const INDIC_FULLBOX: usize = 16;
pub const SCI_DELETERANGE: u32 = 2645;
pub const SCI_REPLACESEL: u32 = 2170;
pub const SCI_LINEDUPLICATE: u32 = 2404;
pub const SCI_SELECTIONDUPLICATE: u32 = 2469;
pub const SCI_MOVESELECTEDLINESUP: u32 = 2620;
pub const SCI_MOVESELECTEDLINESDOWN: u32 = 2621;
pub const SCI_UPPERCASE: u32 = 2341;
pub const SCI_LOWERCASE: u32 = 2340;
pub const SCI_WORDSTARTPOSITION: u32 = 2266;
pub const SCI_WORDENDPOSITION: u32 = 2267;
pub const SCI_SELECTALL: u32 = 2013;
pub const SCI_CANPASTE: u32 = 2173;
pub const SCI_CUT: u32 = 2177;
pub const SCI_COPY: u32 = 2178;
pub const SCI_PASTE: u32 = 2179;
pub const SCI_SETRECTANGULARSELECTIONMODIFIER: u32 = 2598;
pub const SCMOD_ALT: usize = 4;
pub const SCI_GETCHARAT: u32 = 2007;
pub const SCI_GETCURRENTPOS: u32 = 2008;
pub const SCI_GETANCHOR: u32 = 2009;
pub const SCI_REDO: u32 = 2011;
pub const SCI_SETUNDOCOLLECTION: u32 = 2012;
pub const SCI_SETSAVEPOINT: u32 = 2014;
pub const SCI_CANREDO: u32 = 2016;
pub const SCI_GOTOLINE: u32 = 2024;
pub const SCI_GOTOPOS: u32 = 2025;
pub const SCI_SETANCHOR: u32 = 2026;
pub const SCI_SETEOLMODE: u32 = 2031;
pub const SCI_CONVERTEOLS: u32 = 2029;
pub const SCI_SETTABWIDTH: u32 = 2036;
pub const SCI_SETCODEPAGE: u32 = 2037;
pub const SCI_GETTEXTRANGEFULL: u32 = 2039;
pub const SCI_STYLECLEARALL: u32 = 2050;
pub const SCI_STYLESETFORE: u32 = 2051;
pub const SCI_STYLESETBACK: u32 = 2052;
pub const SCI_STYLESETBOLD: u32 = 2053;
pub const SCI_STYLESETITALIC: u32 = 2054;
pub const SCI_STYLESETFONT: u32 = 2056;
pub const SCI_STYLESETSIZEFRACTIONAL: u32 = 2061;
pub const SCI_BEGINUNDOACTION: u32 = 2078;
pub const SCI_ENDUNDOACTION: u32 = 2079;
pub const SCI_SETUSETABS: u32 = 2124;
pub const SCI_GETLINEENDPOSITION: u32 = 2136;
pub const SCI_GETREADONLY: u32 = 2140;
pub const SCI_SETCURRENTPOS: u32 = 2141;
pub const SCI_GETFIRSTVISIBLELINE: u32 = 2152;
pub const SCI_GETLINECOUNT: u32 = 2154;
pub const SCI_SETMARGINLEFT: u32 = 2155;
pub const SCI_SETMARGINRIGHT: u32 = 2157;
pub const SCI_GETMODIFY: u32 = 2159;
pub const SCI_SETSEL: u32 = 2160;
pub const SCI_LINEFROMPOSITION: u32 = 2166;
pub const SCI_POSITIONFROMLINE: u32 = 2167;
pub const SCI_SCROLLCARET: u32 = 2169;
pub const SCI_SETREADONLY: u32 = 2171;
pub const SCI_CANUNDO: u32 = 2174;
pub const SCI_EMPTYUNDOBUFFER: u32 = 2175;
pub const SCI_UNDO: u32 = 2176;
pub const SCI_SETTEXT: u32 = 2181;
pub const SCI_GETTEXT: u32 = 2182;
pub const SCI_REPLACETARGET: u32 = 2194;
pub const SCI_ENSUREVISIBLE: u32 = 2232;
pub const SCI_SETMARGINTYPEN: u32 = 2240;
pub const SCI_SETMARGINWIDTHN: u32 = 2242;
pub const SCI_SETWRAPMODE: u32 = 2268;
pub const SCI_SETLAYOUTCACHE: u32 = 2272;
pub const SCI_TEXTWIDTH: u32 = 2276;
pub const SCI_SETENDATLASTLINE: u32 = 2277;
pub const SCI_SETFOLDMARGINCOLOUR: u32 = 2290;
pub const SCI_SETFOLDMARGINHICOLOUR: u32 = 2291;
pub const SCI_GETDOCPOINTER: u32 = 2357;
pub const SCI_SETDOCPOINTER: u32 = 2358;
pub const SCI_SETMODEVENTMASK: u32 = 2359;
pub const SCI_CREATEDOCUMENT: u32 = 2375;
pub const SCI_ADDREFDOCUMENT: u32 = 2376;
pub const SCI_RELEASEDOCUMENT: u32 = 2377;
pub const SCI_SETFOCUS: u32 = 2380;
pub const SCI_SETXOFFSET: u32 = 2397;
pub const SCI_GETXOFFSET: u32 = 2398;
pub const SCI_GRABFOCUS: u32 = 2400;
pub const SCI_SETSELECTIONMODE: u32 = 2422;
pub const SCI_GETSELECTIONMODE: u32 = 2423;
pub const SCI_GETMOVEEXTENDSSELECTION: u32 = 2706;
pub const SCI_SETCARETSTYLE: u32 = 2512;
pub const SCI_SETSCROLLWIDTHTRACKING: u32 = 2516;
pub const SCI_SETEXTRAASCENT: u32 = 2525;
pub const SCI_SETMULTIPLESELECTION: u32 = 2563;
pub const SCI_SETADDITIONALSELECTIONTYPING: u32 = 2565;
pub const SCI_SETRECTANGULARSELECTIONCARET: u32 = 2588;
pub const SCI_GETRECTANGULARSELECTIONCARET: u32 = 2589;
pub const SCI_SETRECTANGULARSELECTIONANCHOR: u32 = 2590;
pub const SCI_GETRECTANGULARSELECTIONANCHOR: u32 = 2591;
pub const SCI_SETVIRTUALSPACEOPTIONS: u32 = 2596;
pub const SCI_SETFIRSTVISIBLELINE: u32 = 2613;
pub const SCI_SETFONTQUALITY: u32 = 2611;
pub const SCI_SETTECHNOLOGY: u32 = 2630;
pub const SCI_CREATELOADER: u32 = 2632;
pub const SCI_COUNTCHARACTERS: u32 = 2633;
pub const SCI_SETMOUSESELECTIONRECTANGULARSWITCH: u32 = 2668;
pub const SCI_SETIMEINTERACTION: u32 = 2679;
pub const SCI_USEPOPUP: u32 = 2371;
pub const SCI_SETTARGETRANGE: u32 = 2686;
pub const SCI_SETIDLESTYLING: u32 = 2692;
pub const SCI_SETELEMENTCOLOUR: u32 = 2753;
pub const SCI_SETSELECTIONLAYER: u32 = 2763;
pub const SCI_ADDTEXT: u32 = 2001;
pub const SCI_GETEOLMODE: u32 = 2030;
pub const SCI_SETVIEWWS: u32 = 2021;
pub const SCI_GETTABWIDTH: u32 = 2121;
pub const SCI_SETINDENT: u32 = 2122;
pub const SCI_GETLINEINDENTPOSITION: u32 = 2128;
pub const SCI_SETCARETLINEVISIBLE: u32 = 2096;
pub const SCI_GETMARGINLEFT: u32 = 2156;
pub const SCI_GETMARGINRIGHT: u32 = 2158;
pub const SCI_GETMARGINWIDTHN: u32 = 2243;
pub const SCI_BRACEHIGHLIGHT: u32 = 2351;
pub const SCI_BRACEBADLIGHT: u32 = 2352;
pub const SCI_BRACEMATCH: u32 = 2353;
pub const SCI_SETVIEWEOL: u32 = 2356;
pub const SCI_SETEDGECOLUMN: u32 = 2361;
pub const SCI_SETEDGEMODE: u32 = 2363;
pub const SCI_SETEDGECOLOUR: u32 = 2365;
pub const SCI_SETZOOM: u32 = 2373;
pub const SCI_GETZOOM: u32 = 2374;
pub const SCI_SETWRAPINDENTMODE: u32 = 2472;
pub const SCI_GETSELECTIONS: u32 = 2570;
pub const SCI_COLOURISE: u32 = 4003;
pub const SCI_SETPROPERTY: u32 = 4004;
pub const SCI_SETKEYWORDS: u32 = 4005;
pub const SCI_SETILEXER: u32 = 4033;

// --- 値 ---
pub const SC_EOL_CRLF: usize = 0;
pub const SC_EOL_CR: usize = 1;
pub const SC_EOL_LF: usize = 2;
pub const SC_CP_UTF8: usize = 65001;
pub const SC_IME_WINDOWED: usize = 0;
pub const SC_IME_INLINE: usize = 1;
pub const SC_MARGIN_NUMBER: isize = 1;
pub const STYLE_DEFAULT: usize = 32;
pub const STYLE_LINENUMBER: usize = 33;
pub const STYLE_BRACELIGHT: usize = 34;
pub const STYLE_BRACEBAD: usize = 35;
pub const STYLE_CONTROLCHAR: usize = 36;
pub const STYLE_INDENTGUIDE: usize = 37;
pub const SC_ELEMENT_SELECTION_TEXT: usize = 10;
pub const SC_ELEMENT_SELECTION_BACK: usize = 11;
pub const SC_ELEMENT_SELECTION_ADDITIONAL_TEXT: usize = 12;
pub const SC_ELEMENT_SELECTION_ADDITIONAL_BACK: usize = 13;
pub const SC_ELEMENT_SELECTION_SECONDARY_TEXT: usize = 14;
pub const SC_ELEMENT_SELECTION_SECONDARY_BACK: usize = 15;
pub const SC_ELEMENT_SELECTION_INACTIVE_TEXT: usize = 16;
pub const SC_ELEMENT_SELECTION_INACTIVE_BACK: usize = 17;
pub const SC_ELEMENT_CARET: usize = 40;
pub const SC_ELEMENT_CARET_ADDITIONAL: usize = 41;
pub const SC_ELEMENT_CARET_LINE_BACK: usize = 50;
pub const SC_ELEMENT_WHITE_SPACE: usize = 60;
pub const SC_LAYER_BASE: usize = 0;
pub const SC_SEL_STREAM: usize = 0;
pub const SC_SEL_RECTANGLE: usize = 1;
pub const SC_SEL_LINES: usize = 2;
pub const SC_SEL_THIN: usize = 3;
pub const SC_TECHNOLOGY_DEFAULT: usize = 0;
pub const SC_TECHNOLOGY_DIRECTWRITE: usize = 1;
pub const SC_WRAP_NONE: usize = 0;
pub const SC_WRAP_WORD: usize = 1;
pub const SC_WRAP_CHAR: usize = 2;
pub const SC_WRAPINDENT_SAME: usize = 1;
pub const SCWS_INVISIBLE: usize = 0;
pub const SCWS_VISIBLEALWAYS: usize = 1;
pub const EDGE_NONE: usize = 0;
pub const EDGE_LINE: usize = 1;
pub const SC_CACHE_PAGE: usize = 2;
pub const SC_EFF_QUALITY_LCD_OPTIMIZED: usize = 3;
pub const SC_IDLESTYLING_TOVISIBLE: usize = 1;
pub const SC_DOCUMENTOPTION_DEFAULT: isize = 0;
pub const SC_DOCUMENTOPTION_TEXT_LARGE: isize = 0x100;
pub const SC_MOD_INSERTTEXT: c_int = 0x1;
pub const SC_MOD_DELETETEXT: c_int = 0x2;
pub const SC_PERFORMED_UNDO: c_int = 0x20;
pub const SC_PERFORMED_REDO: c_int = 0x40;
pub const SC_UPDATE_CONTENT: c_int = 0x1;
pub const SC_UPDATE_SELECTION: c_int = 0x2;
pub const SCVS_RECTANGULARSELECTION: usize = 1;

// --- 通知 ---
pub const SCN_CHARADDED: u32 = 2001;
pub const SCN_SAVEPOINTREACHED: u32 = 2002;
pub const SCN_ZOOM: u32 = 2018;
pub const SCN_SAVEPOINTLEFT: u32 = 2003;
pub const SCN_UPDATEUI: u32 = 2007;
pub const SCN_MODIFIED: u32 = 2008;
pub const SCN_FOCUSIN: u32 = 2028;
pub const SCN_FOCUSOUT: u32 = 2029;

/// `Sci_NotifyHeader`（Win32 の `NMHDR` と同じ形）。
#[repr(C)]
pub struct NotifyHeader {
    pub hwnd_from: *mut c_void,
    pub id_from: usize,
    pub code: u32,
}

/// `SCNotification`。
#[repr(C)]
pub struct ScNotification {
    pub nmhdr: NotifyHeader,
    pub position: isize,
    pub ch: c_int,
    pub modifiers: c_int,
    pub modification_type: c_int,
    pub text: *const c_char,
    pub length: isize,
    pub lines_added: isize,
    pub message: c_int,
    pub w_param: usize,
    pub l_param: isize,
    pub line: isize,
    pub fold_level_now: c_int,
    pub fold_level_prev: c_int,
    pub margin: c_int,
    pub list_type: c_int,
    pub x: c_int,
    pub y: c_int,
    pub token: c_int,
    pub annotation_lines_added: isize,
    pub updated: c_int,
    pub list_completion_method: c_int,
    pub character_source: c_int,
}

/// `Sci_TextRangeFull`。
#[repr(C)]
pub struct TextRangeFull {
    pub cp_min: isize,
    pub cp_max: isize,
    pub text: *mut c_char,
}

/// Scintilla の `ILoader`（C++ の純粋仮想クラス。仮想デストラクタはないので
/// vtable は宣言順の 3 関数だけ）。x64 では `SCI_METHOD`（`__stdcall`）は無視され、
/// `this` を第 1 引数に取る通常の呼び出し規約になる。
#[repr(C)]
pub struct ILoader {
    pub vtbl: *const ILoaderVtbl,
}

#[repr(C)]
pub struct ILoaderVtbl {
    pub release: unsafe extern "system" fn(this: *mut ILoader) -> c_int,
    pub add_data:
        unsafe extern "system" fn(this: *mut ILoader, data: *const c_char, length: isize) -> c_int,
    pub convert_to_document: unsafe extern "system" fn(this: *mut ILoader) -> *mut c_void,
}

/// `SC_STATUS_OK`。
pub const SC_STATUS_OK: c_int = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_layout_matches_the_c_header() {
        // x64: ヘッダー 24、position 8、int×3 で 12＋詰め物 4、text 8、length 8、
        // linesAdded 8、message 4＋4、wParam 8、lParam 8、line 8、int×7 で 28＋4、
        // annotationLinesAdded 8、int×3 で 12＋4。
        assert_eq!(std::mem::size_of::<NotifyHeader>(), 24);
        assert_eq!(std::mem::offset_of!(ScNotification, position), 24);
        assert_eq!(std::mem::offset_of!(ScNotification, modification_type), 40);
        assert_eq!(std::mem::offset_of!(ScNotification, text), 48);
        assert_eq!(std::mem::offset_of!(ScNotification, length), 56);
        assert_eq!(std::mem::offset_of!(ScNotification, lines_added), 64);
        assert_eq!(std::mem::offset_of!(ScNotification, line), 96);
        assert_eq!(
            std::mem::offset_of!(ScNotification, annotation_lines_added),
            136
        );
        assert_eq!(std::mem::offset_of!(ScNotification, updated), 144);
        assert_eq!(std::mem::size_of::<ScNotification>(), 160);
    }
}

// --- 印刷 ---
pub const SCI_FORMATRANGEFULL: u32 = 2777;
pub const SCI_SETPRINTMAGNIFICATION: u32 = 2146;
pub const SCI_SETPRINTCOLOURMODE: u32 = 2148;
pub const SCI_GETPRINTCOLOURMODE: u32 = 2149;
pub const SCI_SETPRINTWRAPMODE: u32 = 2406;
pub const SCI_GETPRINTWRAPMODE: u32 = 2407;
pub const SC_PRINT_BLACKONWHITE: usize = 2;
pub const SC_PRINT_COLOURONWHITE: usize = 3;

/// `Sci_Rectangle`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SciRectangle {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// `Sci_RangeToFormatFull`。
#[repr(C)]
pub struct RangeToFormatFull {
    pub hdc: *mut c_void,
    pub hdc_target: *mut c_void,
    pub rc: SciRectangle,
    pub rc_page: SciRectangle,
    pub cp_min: isize,
    pub cp_max: isize,
}

// --- ブックマークと行の整理（Scintilla.h と照合済み） ---
pub const SCI_MARKERDEFINE: u32 = 2040;
pub const SCI_MARKERSETFORE: u32 = 2041;
pub const SCI_MARKERSETBACK: u32 = 2042;
pub const SCI_MARKERADD: u32 = 2043;
pub const SCI_MARKERDELETE: u32 = 2044;
pub const SCI_MARKERDELETEALL: u32 = 2045;
pub const SCI_MARKERGET: u32 = 2046;
pub const SCI_MARKERNEXT: u32 = 2047;
pub const SCI_MARKERPREVIOUS: u32 = 2048;
pub const SCI_SETMARGINMASKN: u32 = 2244;
pub const SCI_GETSELECTIONNSTART: u32 = 2585;
pub const SCI_GETSELECTIONNEND: u32 = 2587;
pub const SC_MARK_CIRCLE: isize = 0;
pub const SC_MARGIN_SYMBOL: isize = 0;
