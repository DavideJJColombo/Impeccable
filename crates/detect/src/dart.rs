//! The Dart text-engine pass for `.dart` sources (Flutter widget code). The
//! `detect_text` engine keeps its web-shaped built-ins — nearly all gated on
//! CSS/Tailwind/JSX tokens a Dart line never carries — and hands Dart files to
//! this module: a string-aware comment stripper and a matcher table that
//! speaks Dart tokens instead. `Curves.bounceOut` instead of `cubic-bezier(…,
//! y > 1.1)`, `fontFamily: 'Inter'` and `GoogleFonts.spaceGroteskTextTheme()`
//! instead of `font-family: Inter`, `Color(0xFF9C27B0)` instead of `#9c27b0`,
//! and `Border(left: BorderSide(...))` instead of `border-left: 4px solid`.
//!
//! Every matcher targets an existing registry id so the rule name, category,
//! and description stay shared with the web engines; only the trigger syntax
//! differs. Roboto is deliberately absent from the font list here: on Android
//! Material it is the system face (`skill/reference/android.md`), so an
//! explicit `Roboto` choice in Dart is not the drift tell it is on the web.
//! The shared dedupe and the inline waivers in `detect_text` apply to these
//! findings like to any other text-engine finding, so `// impeccable-disable`
//! works in Dart files untouched.
//!
//! Deliberately text-only: layout rules (nested cards, touch targets, type
//! hierarchy, line length) need rendered geometry and stay with the browser
//! engine and the LLM guidance in `skill/reference/flutter.md`.

use impeccable_core::findings::{finding, Finding};
use impeccable_core::js::ci;

use crate::regex_matchers::{all, MatchCtx};
use crate::util::{re, B, D, WS};

/// Case-insensitive alternation over exact names, the way
/// `OVERUSED_FONT_RE` builds its list.
fn ci_alts(names: &[&str]) -> String {
    names.iter().map(|n| ci(n)).collect::<Vec<_>>().join("|")
}

// ─── Comment and string stripping ────────────────────────────────────────────

/// Blank `//` and `/* */` comments outside string literals so matcher lines
/// see code, not chatter. Spaces replace the comment bytes to keep columns,
/// newlines keep line numbers, and string literals survive verbatim — their
/// contents are UI copy, and `'https://example.com'` inside a string is a
/// URL, never a comment opener. Handles `'`/`"` single and triple quotes,
/// raw `r'…'` strings, `\` escapes, and `${…}` interpolation (quotes inside
/// an interpolation expression do not terminate the string).
pub fn strip_dart_comments(content: &str) -> String {
    let chars: Vec<char> = content.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    while i < n {
        let ch = chars[i];
        if ch == '/' && i + 1 < n {
            match chars[i + 1] {
                '/' => {
                    while i < n && chars[i] != '\n' {
                        out.push(' ');
                        i += 1;
                    }
                    continue; // the newline itself is ordinary text
                }
                '*' => {
                    out.push_str("  ");
                    i += 2;
                    while i < n {
                        if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                            out.push_str("  ");
                            i += 2;
                            break;
                        }
                        if chars[i] == '\n' {
                            out.push('\n');
                        } else {
                            out.push(' ');
                        }
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }
        // String openers: a plain quote, or `r` immediately before one (raw).
        let raw = ch == 'r' && i + 1 < n && (chars[i + 1] == '\'' || chars[i + 1] == '"');
        if ch == '\'' || ch == '"' || raw {
            if raw {
                out.push('r');
                i += 1;
            }
            let q = chars[i];
            let triple = i + 2 < n && chars[i + 1] == q && chars[i + 2] == q;
            let q_len = if triple { 3 } else { 1 };
            for _ in 0..q_len {
                out.push(q);
            }
            i += q_len;
            let mut brace_depth = 0usize; // inside `${ … }`
            loop {
                if i >= n {
                    break;
                }
                let c = chars[i];
                if brace_depth == 0 && c == '$' && i + 1 < n && chars[i + 1] == '{' {
                    out.push_str("${");
                    i += 2;
                    brace_depth = 1;
                    continue;
                }
                if brace_depth > 0 {
                    if c == '{' {
                        brace_depth += 1;
                    } else if c == '}' {
                        brace_depth -= 1;
                    }
                    out.push(c);
                    i += 1;
                    continue;
                }
                if !raw && c == '\\' && i + 1 < n {
                    out.push(c);
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == q {
                    if triple {
                        if i + 2 < n && chars[i + 1] == q && chars[i + 2] == q {
                            out.push(q);
                            out.push(q);
                            out.push(q);
                            i += 3;
                            break;
                        }
                    } else {
                        out.push(q);
                        i += 1;
                        break;
                    }
                }
                out.push(c);
                i += 1;
            }
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

// ─── Dart color helpers ──────────────────────────────────────────────────────

re!(
    HEX_COLOR_RE,
    format!("Color{WS}*\\({WS}*0[xX]([0-9a-fA-F]{{8}})")
);

re!(NAMED_COLOR_RE, r"(?-u:\b)Colors\.([a-zA-Z][a-zA-Z0-9]*)");

fn hex_channel(s: &str) -> f64 {
    u32::from_str_radix(s, 16).map(|v| v as f64).unwrap_or(0.0)
}

/// Flutter `Color(0xAARRGGBB)` — alpha first, unlike web hex strings.
fn rgb_from_hex(digits: &str) -> Option<[f64; 3]> {
    if digits.len() != 8 {
        return None;
    }
    Some([
        hex_channel(&digits[2..4]),
        hex_channel(&digits[4..6]),
        hex_channel(&digits[6..8]),
    ])
}

fn spread(c: [f64; 3]) -> f64 {
    collapse(c, true) - collapse(c, false)
}

fn collapse(c: [f64; 3], max: bool) -> f64 {
    let m = c[0].max(c[1]).max(c[2]);
    let n = c[0].min(c[1]).min(c[2]);
    if max {
        m
    } else {
        n
    }
}

fn hue_sat(c: [f64; 3]) -> (f64, f64) {
    let max = collapse(c, true);
    let min = collapse(c, false);
    if max <= 0.0 {
        return (0.0, 0.0);
    }
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0);
    }
    let h = if max == c[0] {
        ((c[1] - c[2]) / d) % 6.0
    } else if max == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    };
    let mut h = h * 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, d / max)
}

/// Purples and indigos — the `ai-color-palette` hue band, the same families
/// the web matchers key on (`text-purple-*`, `text-violet-*`, `text-indigo-*`).
fn is_purple_hex(c: [f64; 3]) -> bool {
    let (h, s) = hue_sat(c);
    s >= 0.2 && h >= 225.0 && h <= 322.0
}

/// Cream/beige — light, warm, tinted toward red. Grays and pure white fail
/// the r−b gap; a saturated mid tone fails the lightness floor.
fn is_cream_hex(c: [f64; 3]) -> bool {
    let (r, g, b) = (c[0], c[1], c[2]);
    collapse(c, false) >= 200.0 && (r - b) >= 10.0 && r >= g && g >= b
}

const PURPLE_FAMILY_NAMES: &[&str] = &[
    "purple",
    "purpleAccent",
    "deepPurple",
    "deepPurpleAccent",
    "indigo",
    "indigoAccent",
];

/// Neutral Material hue families, by name: white/black shades, grays, and the
/// low-saturation slate `blueGrey` are never chromatic for these rules.
fn named_color_class(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let neutral = ["white", "black", "grey", "gray", "bluegrey", "transparent"];
    if neutral.iter().any(|n| lower.starts_with(n)) {
        "neutral"
    } else if PURPLE_FAMILY_NAMES.contains(&name) {
        "purple"
    } else {
        "chromatic"
    }
}

/// A second chromatic candidate in the window: another named Material color
/// outside the purple family and the neutrals, or a chromatic hex literal.
fn second_chromatic(window: &str, skip: &str) -> Option<String> {
    for cap in NAMED_COLOR_RE.captures_iter(window) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if name == skip || named_color_class(name) != "chromatic" {
            continue;
        }
        return Some(format!("Colors.{name}"));
    }
    for cap in HEX_COLOR_RE.captures_iter(window) {
        let digits = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if let Some(rgb) = rgb_from_hex(digits) {
            if spread(rgb) >= 30.0 {
                return Some(format!("Color(0x{digits})"));
            }
        }
    }
    None
}

// ─── Shared line/window shapes ───────────────────────────────────────────────

re!(HAS_GRADIENT_RE, "Gradient");
re!(HAS_COLORS_LIST_RE, format!("{B}colors{WS}*:"));
re!(BORDER_OPEN_RE, format!("{B}Border{WS}*\\("));
re!(RADIUS_RE, format!("{B}BorderRadius{B}"));
re!(
    FONT_SIZE_CUE_RE,
    format!("fontSize{WS}*:{WS}*(?:{A}|{B_}){B}", A = "[2-9][2-9]", B_ = "[1-9][0-9][0-9]")
);

/// The two dope sheets a matcher may consult: the matcher line alone, and a
/// few surrounding lines joined with spaces (the same ±3-line join
/// `run_regex_matchers` gives block-context matchers).
#[derive(Debug, Clone)]
pub struct LineWindows {
    pub line: String,
    pub near: String,
    pub wide: String,
}

impl LineWindows {
    fn new(lines: &[&str], i: usize) -> LineWindows {
        let near = window_of(lines, i);
        let wide = window_wide_of(lines, i);
        LineWindows {
            line: lines[i].to_string(),
            near,
            wide,
        }
    }
}

fn window_of(lines: &[&str], i: usize) -> String {
    let lo = i.saturating_sub(3);
    let hi = (i + 4).min(lines.len());
    lines[lo..hi].join(" ")
}

fn window_wide_of(lines: &[&str], i: usize) -> String {
    let lo = i.saturating_sub(4);
    let hi = (i + 5).min(lines.len());
    lines[lo..hi].join(" ")
}

// ─── The matchers ────────────────────────────────────────────────────────────

/// One Dart matcher row. Like the web table, `find_all` runs per line and
/// `test`/`fmt` receive the match, its line, and the surrounding windows
/// (`ScanLine`).
struct DartMatcher {
    id: &'static str,
    find_all: fn(&str) -> Vec<MatchCtx>,
    test: for<'a> fn(&MatchCtx, &ScanLine<'a>) -> bool,
    fmt: for<'a> fn(&MatchCtx, &ScanLine<'a>) -> String,
}

// bounce-easing ─ Curves.bounceOut and friends, the Flutter cubic-bezier(…, y
// > 1.1): overshoot curves read as bounce on real hardware.
re!(
    BOUNCE_CURVE_RE,
    format!(
        "{B}Curves\\.({}|{}|{}|{}|{}|{}|{}){B}",
        ci("bounceOut"),
        ci("bounceIn"),
        ci("bounceInOut"),
        ci("elasticOut"),
        ci("elasticIn"),
        ci("elasticInOut"),
        ci("easeOutBack")
    )
);

fn bounce_find(l: &str) -> Vec<MatchCtx> {
    all(&BOUNCE_CURVE_RE, l)
}

fn always_true(_: &MatchCtx, _: &ScanLine) -> bool {
    true
}

// overused-font ─ `fontFamily: 'Inter'`-style properties. `fontFamilyFallback
// : [...]` cannot match: after `fontFamily` the pattern requires the colon.
const OVERUSED_FONT_NAMES: &[&str] = &[
    "Plus Jakarta Sans",
    "Instrument Serif",
    "Instrument Sans",
    "Space Grotesk",
    "Geist Sans",
    "Geist Mono",
    "Mona Sans",
    "Open Sans",
    "Montserrat",
    "Helvetica",
    "Fraunces",
    "Arial",
    "Geist",
    "Recoleta",
    "Lato",
    "Inter",
];
// Deliberately no Roboto: the Android system face, a Dart default, not a tell.

re!(
    FONT_FAMILY_DART_RE,
    format!("{B}{}{WS}*:{WS}*['\"]({})['\"]", ci("fontFamily"), ci_alts(OVERUSED_FONT_NAMES))
);

fn font_family_find(l: &str) -> Vec<MatchCtx> {
    all(&FONT_FAMILY_DART_RE, l)
}

// overused-font ─ the google_fonts package: `GoogleFonts.inter()`,
// `GoogleFonts.plusJakartaSansTextTheme()`. The tail class keeps partially
// overlapped ids (`.inter` inside `.interTextTheme` is fine; a longer camel
// name that merely starts with a shorter one) from matching the short name.
re!(
    GOOGLE_FONTS_DART_RE,
    format!(
        "{B}GoogleFonts\\.({})(?:[^a-z_0-9]|$)",
        ci_alts(&[
            "plusJakartaSans",
            "instrumentSerif",
            "instrumentSans",
            "spaceGrotesk",
            "geistSans",
            "geistMono",
            "monaSans",
            "openSans",
            "montserrat",
            "helvetica",
            "fraunces",
            "arial",
            "recoleta",
            "lato",
            "geist",
            "inter",
        ])
    )
);

fn google_fonts_find(l: &str) -> Vec<MatchCtx> {
    all(&GOOGLE_FONTS_DART_RE, l)
}

fn google_fonts_display(camel: &str) -> String {
    match camel.to_ascii_lowercase().as_str() {
        "plusjakartasans" => "Plus Jakarta Sans",
        "instrumentserif" => "Instrument Serif",
        "instrumentsans" => "Instrument Sans",
        "spacegrotesk" => "Space Grotesk",
        "geistsans" => "Geist Sans",
        "geistmono" => "Geist Mono",
        "monasans" => "Mona Sans",
        "opensans" => "Open Sans",
        "montserrat" => "Montserrat",
        "helvetica" => "Helvetica",
        "fraunces" => "Fraunces",
        "arial" => "Arial",
        "recoleta" => "Recoleta",
        "lato" => "Lato",
        "geist" => "Geist",
        "inter" => "Inter",
        _ => camel,
    }
    .to_string()
}

// ai-color-palette ─ purple gradient lists: `colors: [Colors.deepPurple,
// Colors.blueAccent]` inside a LinearGradient/RadialGradient/SweepGradient —
// the Dart twin of `from-purple-… to-blue-…`. Both tokens must be present,
// the list context keeps a lone brand purple quiet, and the window keeps
// wrapped lists in reach.
fn gradient_purple_find(l: &str) -> Vec<MatchCtx> {
    all(&NAMED_COLOR_RE, l)
}

fn gradient_purple_test(m: &MatchCtx, scan: &ScanLine) -> bool {
    let w = &scan.windows;
    let name = m.g(1);
    if named_color_class(name) != "purple" {
        return false;
    }
    HAS_GRADIENT_RE.is_match(&w.wide)
        && HAS_COLORS_LIST_RE.is_match(&w.wide)
        && second_chromatic(&w.wide, name).is_some()
}

// ai-color-palette ─ purple on a heading: hex literal or named purple inside
// a large-text context (headlines, display roles, titleLarge, or an explicit
// big fontSize on the TextStyle the color feeds).
re!(
    HEADING_CUE_RE,
    format!(
        "{B}({}|{}|{HS}|{DL}|{DM}|{DS}|{TL}){B}",
        ci("headlineLarge"),
        ci("headlineMedium"),
        HS = ci("headlineSmall"),
        DL = ci("displayLarge"),
        DM = ci("displayMedium"),
        DS = ci("displaySmall"),
        TL = ci("titleLarge")
    )
);

/// The heading cues a purple may not sit on: the headline and display roles
/// plus `titleLarge`; `titleSmall`/`titleMedium` are body-label sized, not
/// headings, and an explicit `fontSize` ≥ 22 counts too.
fn heading_cue(scan: &ScanLine) -> bool {
    let w = &scan.windows;
    HEADING_CUE_RE.is_match(&w.wide) || FONT_SIZE_CUE_RE.is_match(&w.wide)
}

fn heading_purple_hex_find(l: &str) -> Vec<MatchCtx> {
    all(&HEX_COLOR_RE, l)
}

fn heading_purple_hex_test(m: &MatchCtx, scan: &ScanLine) -> bool {
    rgb_from_hex(m.g(1))
        .map(is_purple_hex)
        .unwrap_or(false)
        && heading_cue(scan)
}

fn heading_purple_named_find(l: &str) -> Vec<MatchCtx> {
    all(&NAMED_COLOR_RE, l)
}

fn heading_purple_named_test(m: &MatchCtx, scan: &ScanLine) -> bool {
    named_color_class(m.g(1)) == "purple" && heading_cue(scan)
}

// cream-palette ─ the warm off-white default surface reached for by reflex:
// `scaffoldBackgroundColor: Color(0xFFFDF6EC)` or a like `backgroundColor:`.
re!(
    CREAM_BG_RE,
    format!(
        "{B}(?:{}|{}){WS}*:{WS}*(?:{}{WS}+)?Color{WS}*\\({WS}*0[xX]([0-9a-fA-F]{{8}})",
        ci("scaffoldBackgroundColor"),
        ci("backgroundColor"),
        ci("const")
    )
);

fn cream_find(l: &str) -> Vec<MatchCtx> {
    all(&CREAM_BG_RE, l)
}

fn cream_test(m: &MatchCtx, _: &ScanLine) -> bool {
    rgb_from_hex(m.g(1))
        .map(is_cream_hex)
        .unwrap_or(false)
}

// side-tab ─ a one-sided thick chromatic border: `Border(left: BorderSide(color:
// …, width: 4))` on a container that is not rounded. The member marker must
// sit before the BorderSide on the same line (`left: BorderSide(`), the
// window must show a `Border(` opening with no `.all` and no BorderRadius,
// exactly one side member, and the BorderSide itself a chromatic stroke
// of width ≥ 3.
re!(
    MEMBER_SIDE_RE,
    format!("{B}(left|right|start|end){WS}*:{WS}*BorderSide")
);
re!(
    SIDE_TAB_DART_RE,
    format!("{B}(left|right|start|end){WS}*:{WS}*BorderSide{WS}*\\(")
);

fn side_tab_find(l: &str) -> Vec<MatchCtx> {
    all(&SIDE_TAB_DART_RE, l)
}

/// The match's position and its file line context, so a matcher can read
/// across later lines for arguments that wrap. `SideText::of` returns the
/// current line plus up to two following lines joined with a space — the
/// current line first, so byte offsets of matches on it stay valid.
#[derive(Debug, Clone)]
pub struct ScanLine<'a> {
    pub i: usize,
    pub lines: &'a [&'a str],
    pub windows: LineWindows,
}

pub fn side_text(scan: &ScanLine) -> String {
    let hi = (scan.i + 3).min(scan.lines.len());
    scan.lines[scan.i..hi].join(" ")
}

fn side_tab_test(m: &MatchCtx, scan: &ScanLine) -> bool {
    let w = &scan.windows;
    // window sanity: a Border( opening, not Border.all, not rounded
    if !BORDER_OPEN_RE.is_match(&w.near) || w.near.contains(".all") || RADIUS_RE.is_match(&w.wide) {
        return false;
    }
    // one-sided: exactly one side member in the window
    if MEMBER_SIDE_RE.find_iter(&w.wide).count() > 1 {
        return false;
    }
    let args = parse_border_side_args(m, &side_text(scan));
    matches!(args, Some((_, width)) if width >= 3.0)
}

/// Reads the balanced `BorderSide(…)` argument text at the match and returns
/// `(color label, width)` when the stroke is chromatic. Width defaults to
/// Flutter's implicit `1.0` when the argument is absent.
fn parse_border_side_args(m: &MatchCtx, text: &str) -> Option<(String, f64)> {
    let start = m.index + m.whole().len();
    let rest = &text[start.min(text.len())..];
    let mut depth = 0usize;
    let mut seg = String::new();
    for c in rest.chars() {
        if c == ')' {
            if depth == 0 {
                break;
            }
            depth -= 1;
            seg.push(c);
            continue;
        }
        if c == '(' {
            depth += 1;
        }
        seg.push(c);
    }
    if let Some(d) = HEX_COLOR_RE.captures(&seg).and_then(|c| c.get(1)) {
        let rgb = rgb_from_hex(d.as_str())?;
        if spread(rgb) < 30.0 {
            return None; // neutral stroke — not an accent
        }
        return finish_side_tab_args(&seg, format!("Color(0x{})", d.as_str()));
    }
    let name = NAMED_COLOR_RE.find(&seg).and_then(|mm| {
        let name = &mm.as_str()["Colors.".len()..];
        match named_color_class(name) {
            "neutral" | "purple" => None, // purple reserves itself: not a side stripe
            _ => Some(name.to_string()),
        }
    })?;
    finish_side_tab_args(&seg, format!("Colors.{name}"))
}

fn finish_side_tab_args(seg: &str, color: String) -> Option<(String, f64)> {
    let width = BORDER_SIDE_WIDTH_RE
        .captures(seg)
        .and_then(|c| c.get(1))
        .and_then(|mm| mm.as_str().parse::<f64>().ok())
        .unwrap_or(1.0);
    if width < 3.0 {
        return None;
    }
    Some((color, width))
}

re!(
    BORDER_SIDE_WIDTH_RE,
    format!("width{WS}*:{WS}*({D}+)")
);

// ─── The table ───────────────────────────────────────────────────────────────

const DART_MATCHERS: &[DartMatcher] = &[
    DartMatcher {
        id: "bounce-easing",
        find_all: bounce_find,
        test: always_true,
        fmt: |m, _| m.whole().to_string(),
    },
    DartMatcher {
        id: "overused-font",
        find_all: font_family_find,
        test: always_true,
        fmt: |m, _| m.whole().to_string(),
    },
    DartMatcher {
        id: "overused-font",
        find_all: google_fonts_find,
        test: always_true,
        fmt: |m, _| format!("Google Fonts: {}", google_fonts_display(m.g(1))),
    },
    DartMatcher {
        id: "ai-color-palette",
        find_all: gradient_purple_find,
        test: gradient_purple_test,
        fmt: |m, _| format!("{} gradient", m.whole()),
    },
    DartMatcher {
        id: "ai-color-palette",
        find_all: heading_purple_hex_find,
        test: heading_purple_hex_test,
        // The token match ends inside the literal (before the closing paren);
        // rebuild the label from the digits so the snippet reads whole.
        fmt: |m, _| format!("Color(0x{}) on heading", m.g(1)),
    },
    DartMatcher {
        id: "ai-color-palette",
        find_all: heading_purple_named_find,
        test: heading_purple_named_test,
        fmt: |m, _| format!("{} on heading", m.whole()),
    },
    DartMatcher {
        id: "cream-palette",
        find_all: cream_find,
        test: cream_test,
        fmt: |m, _| format!("Color(0x{}) page background", m.g(1)),
    },
    DartMatcher {
        id: "side-tab",
        find_all: side_tab_find,
        test: side_tab_test,
        fmt: |m, scan| match parse_border_side_args(m, &side_text(scan)) {
            Some((color, width)) => {
                let side = m.g(1).to_string();
                format!("Border({side}: BorderSide(color: {color}, width: {width}))")
            }
            None => "one-sided BorderSide".to_string(),
        },
    },
];

/// The Dart pass over one source file. Runs after the shared web matcher pass
/// inside `detect_text`, so all `.dart` findings flow through that engine's
/// dedupe, waivers, and output.
pub fn dart_findings(content: &str, file_path: &str) -> Vec<Finding> {
    let stripped = strip_dart_comments(content);
    let lines: Vec<&str> = stripped.split('\n').collect();
    let mut findings = Vec::new();
    for i in 0..lines.len() {
        let scan = ScanLine {
            i,
            lines: &lines,
            windows: LineWindows::new(&lines, i),
        };
        for matcher in DART_MATCHERS {
            for m in (matcher.find_all)(lines[i]) {
                if (matcher.test)(&m, &scan) {
                    findings.push(finding(
                        matcher.id,
                        file_path,
                        &(matcher.fmt)(&m, &scan),
                        (i + 1) as f64,
                    ));
                }
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids_of(code: &str) -> Vec<(String, f64)> {
        dart_findings(code, "a.dart")
            .into_iter()
            .map(|f| (f.antipattern, f.line))
            .collect()
    }

    fn has_id(code: &str, id: &str) -> bool {
        ids_of(code).iter().any(|(a, _)| a == id)
    }


    #[test]
    fn comments_are_blanked_and_strings_kept() {
        let out = strip_dart_comments(
            "final a = 1; // Curves.bounceOut\nfinal b = 'https://x/y';\n/* multi\nline */ final c = \"keep\";\n",
        );
        assert!(!out.contains("Curves.bounceOut"));
        assert!(out.contains("'https://x/y';"));
        assert!(out.contains("\"keep\";"));
        // Block comment bodies blank out to spaces; newlines survive.
        assert!(!out.contains("multi"));
        assert!(!out.contains("line */"));
        assert_eq!(out.matches('\n').count(), 4);
    }

    #[test]
    fn interpolation_does_not_terminate_strings() {
        let out = strip_dart_comments("final t = 'A ${b('x')} c'; // gone\n");
        assert!(out.contains("'A ${b('x')} c';"));
        assert!(!out.contains("gone"));
    }

    #[test]
    fn bounce_curves_flag_and_neighbors_stay_quiet() {
        assert!(has_id("curve: Curves.bounceOut,", "bounce-easing"));
        assert!(has_id("curve: Curves.bounceInOut,", "bounce-easing"));
        assert!(has_id("curve: Curves.elasticOut,", "bounce-easing"));
        assert!(has_id("curve: Curves.easeOutBack,", "bounce-easing"));
        // Good easings stay quiet, including word-suffix confusables.
        assert!(!has_id("curve: Curves.easeOutCubic,", "bounce-easing"));
        assert!(!has_id("curve: Curves.fastOutSlowIn,", "bounce-easing"));
        assert!(!has_id("curve: Curves.easeInOut,", "bounce-easing"));
        // The Curve token needs a boundary: `noCurves.bounceOut` stays quiet.
        assert!(!has_id("noCurves.bounceOut,", "bounce-easing"));
    }

    #[test]
    fn overused_fonts_flag_without_roboto_or_fallback_noise() {
        assert!(has_id("fontFamily: 'Inter',", "overused-font"));
        assert!(has_id("fontFamily: 'Space Grotesk',", "overused-font"));
        assert!(has_id("GoogleFonts.spaceGroteskTextTheme()", "overused-font"));
        assert!(has_id("GoogleFonts.plusJakartaSans(color: c)", "overused-font"));
        assert!(!has_id("fontFamilyFallback: ['Inter'],", "overused-font"));
        assert!(!has_id("fontFamily: 'Gilroy',", "overused-font"));
        assert!(!has_id("GoogleFonts.robotoTextTheme()", "overused-font"));
        assert!(!has_id("GoogleFonts.interactiveCodeWidget(", "overused-font"));
    }

    #[test]
    fn purple_gradient_flags_but_brand_accent_stays_quiet() {
        let flag = r#"BoxDecoration(
              gradient: LinearGradient(
                colors: [Colors.deepPurple, Colors.blueAccent],
              ),
            )"#;
        assert!(has_id(flag, "ai-color-palette"));
        // A lone named purple is a legitimate brand accent.
        assert!(!has_id("color: Colors.deepPurple,", "ai-color-palette"));
        // A gradient with no purple family member is clean.
        assert!(!has_id(
            "gradient: LinearGradient(colors: [Colors.teal, Colors.blue]),",
            "ai-color-palette"
        ));
        // Purple gradient whose second member is neutral stays quiet.
        assert!(!has_id(
            "gradient: LinearGradient(colors: [Colors.deepPurple, Colors.black26]),",
            "ai-color-palette"
        ));
    }

    #[test]
    fn purple_on_heading_flags_without_heading_cue_quiet() {
        assert!(has_id(
            "headlineLarge: TextStyle(color: const Color(0xFF9C27B0)),",
            "ai-color-palette"
        ));
        assert!(has_id(
            "style: TextStyle(fontSize: 28, color: const Color(0xFF673AB7)),",
            "ai-color-palette"
        ));
        assert!(has_id(
            "displayMedium: TextStyle(color: Colors.deepPurple),",
            "ai-color-palette"
        ));
        // Small text or body label contexts are not headings.
        assert!(!has_id(
            "bodyText: TextStyle(fontSize: 14, color: Colors.deepPurple),",
            "ai-color-palette"
        ));
        assert!(!has_id(
            "titleSmall: TextStyle(color: Colors.deepPurple),",
            "ai-color-palette"
        ));
        // Purple family gate: a blue hex sits quiet regardless.
        assert!(!has_id(
            "headlineLarge: TextStyle(color: const Color(0xFF2196F3)),",
            "ai-color-palette"
        ));
    }

    #[test]
    fn cream_background_flags_and_cool_neutrals_stay_quiet() {
        assert!(has_id(
            "backgroundColor: const Color(0xFFFDF6EC),",
            "cream-palette"
        ));
        assert!(has_id(
            "scaffoldBackgroundColor: Color(0xfff5f5dc),",
            "cream-palette"
        ));
        assert!(!has_id(
            "backgroundColor: const Color(0xFFF8F6F4),",
            "cream-palette"
        ));
        assert!(!has_id(
            "backgroundColor: const Color(0xFF121212),",
            "cream-palette"
        ));
        assert!(!has_id(
            "backgroundColor: Colors.white,",
            "cream-palette"
        ));
    }

    #[test]
    fn one_sided_border_flags_and_uniform_or_neutral_stay_quiet() {
        assert!(has_id(
            "border: Border(left: BorderSide(color: Color(0xFF8B5CF6), width: 4)),",
            "side-tab"
        ));
        assert!(has_id(
            "border: Border(start: BorderSide(color: Colors.teal, width: 6)),",
            "side-tab"
        ));
        // Uniform borders are fine even when chromatic.
        assert!(!has_id(
            "border: Border.all(color: Color(0xFF1B4B5A), width: 4),",
            "side-tab"
        ));
        // Thin strokes are fine.
        assert!(!has_id(
            "border: Border(left: BorderSide(color: Color(0xFF8B5CF6), width: 1)),",
            "side-tab"
        ));
        // Neutral stroke is fine.
        assert!(!has_id(
            "border: Border(left: BorderSide(color: Colors.grey, width: 4)),",
            "side-tab"
        ));
        // Rounded containers never take a one-sided border flag here.
        assert!(!has_id(
            r#"decoration: BoxDecoration(
                borderRadius: BorderRadius.circular(8),
                border: Border(left: BorderSide(color: Color(0xFF8B5CF6), width: 4)),
            )"#,
            "side-tab"
        ));
        // Two-sided border is not a side accent.
        assert!(!has_id(
            "border: Border(left: BorderSide(color: Colors.teal, width: 4), right: BorderSide(color: Colors.teal, width: 4)),",
            "side-tab"
        ));
    }
}
