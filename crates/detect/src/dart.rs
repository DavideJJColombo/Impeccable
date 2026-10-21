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
//! Two views of the source are prepared. Comments are blanked in both so line
//! and column numbers survive. The *font* matchers read the view that keeps
//! string literals verbatim, because `fontFamily: 'Inter'` lives in one. Every
//! other matcher reads a view whose string *contents* are blanked too: a tell
//! token quoted in UI copy (`Text('Curves.bounceOut is banned')`) is copy, not
//! code, and the blanked view is also structurally safe to scan for the
//! enclosing-group reads the context rules below depend on.
//!
//! Context rules read the group that owns a token — its `TextStyle(...)`,
//! `LinearGradient(...)`, or `Border`/`BoxDecoration(...)` — never a fixed
//! line window, so an unrelated widget beside it cannot change the verdict.
//!
//! Deliberately text-only: layout rules (nested cards, touch targets, type
//! hierarchy, line length) need rendered geometry and stay with the browser
//! engine and the LLM guidance in `skill/reference/flutter.md`.

use impeccable_core::checks::rules::SIDE_ACCENT_MIN_RADIUS_PX;
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
/// raw `r'…'` strings, `\` escapes, Dart's nested block comments, and `${…}`
/// interpolation (quotes inside an interpolation expression do not terminate
/// the string; a raw string has no interpolation at all).
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
                    // Dart block comments nest: `/* outer /* inner */ still */`.
                    let mut depth = 1usize;
                    while i < n {
                        if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                            depth += 1;
                            out.push_str("  ");
                            i += 2;
                            continue;
                        }
                        if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                            depth -= 1;
                            out.push_str("  ");
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                            continue;
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
                // Interpolation is a plain-string feature; in `r'…'` the `${`
                // is literal text and must not swallow later comments.
                if !raw && brace_depth == 0 && c == '$' && i + 1 < n && chars[i + 1] == '{' {
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

/// Blank the *contents* of every string literal, keeping the delimiters, the
/// raw `r` marker, newlines, and the byte length. Runs on text that
/// [`strip_dart_comments`] already produced, so no comment can open a string.
fn blank_string_contents(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < n {
        let ch = chars[i];
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
            loop {
                if i >= n {
                    break;
                }
                let c = chars[i];
                if !raw && c == '\\' && i + 1 < n {
                    out.push(if c == '\n' { '\n' } else { ' ' });
                    out.push(if chars[i + 1] == '\n' { '\n' } else { ' ' });
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
                        out.push(' ');
                        i += 1;
                        continue;
                    }
                    out.push(q);
                    i += 1;
                    break;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

// ─── Group helpers ───────────────────────────────────────────────────────────

/// Byte bounds of the innermost `(`/`[`/`{` group that contains `at`, as
/// `(open, close)`: `open` indexes the opener, `close` its matching closer.
/// `None` when the brackets around `at` are unbalanced.
fn group_bounds(text: &str, at: usize) -> Option<(usize, usize)> {
    let at = at.min(text.len());
    let mut stack: Vec<(usize, char)> = Vec::new();
    for (idx, ch) in text.char_indices() {
        if idx >= at {
            break;
        }
        match ch {
            '(' | '[' | '{' => stack.push((idx, ch)),
            ')' | ']' | '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let (open, opener) = *stack.last()?;
    let closer = match opener {
        '(' => ')',
        '[' => ']',
        _ => '}',
    };
    let mut depth = 1usize;
    for (off, ch) in text[open..].char_indices().skip(1) {
        if ch == opener {
            depth += 1;
        } else if ch == closer {
            depth -= 1;
            if depth == 0 {
                return Some((open, open + off));
            }
        }
    }
    None
}

/// The group `level` levels out from the innermost one containing `at`.
/// Level 1 is [`group_bounds`], level 2 its parent, and so on.
fn ancestor_group(text: &str, at: usize, level: usize) -> Option<(usize, usize)> {
    let mut current = group_bounds(text, at)?;
    for _ in 1..level {
        current = group_bounds(text, current.0)?;
    }
    Some(current)
}

/// The identifier immediately before an opener, so `Border(` and
/// `BorderDirectional(` can be told apart from `Border.all(` and any other
/// call that happens to carry a `left:` argument.
fn opener_name(text: &str, open: usize) -> &str {
    let bytes = text.as_bytes();
    let mut start = open.min(bytes.len());
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    &text[start..open.min(text.len())]
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

/// Any non-neutral color in the span besides the matched one: another named
/// Material color (purple families included — a deepPurple→indigo gradient is
/// the same tell as deepPurple→blue), or a chromatic hex literal.
fn second_chromatic(span: &str, skip: &str) -> Option<String> {
    for cap in NAMED_COLOR_RE.captures_iter(span) {
        let name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if name == skip || named_color_class(name) == "neutral" {
            continue;
        }
        return Some(format!("Colors.{name}"));
    }
    for cap in HEX_COLOR_RE.captures_iter(span) {
        let digits = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        if let Some(rgb) = rgb_from_hex(digits) {
            if spread(rgb) >= 30.0 {
                return Some(format!("Color(0x{digits})"));
            }
        }
    }
    None
}

fn num_from_caps(caps: &regex::Captures, i: usize) -> Option<f64> {
    caps.get(i).and_then(|m| m.as_str().parse::<f64>().ok())
}

// ─── Radius → corners ────────────────────────────────────────────────────────

re!(
    RADIUS_DECL_RE,
    format!("{B}{}{B}", ci("borderRadius"))
);
re!(
    RADIUS_ALL_RE,
    format!(
        "{B}BorderRadius(?:Directional)?{WS}*\\.{WS}*(?:{}|{}){WS}*\\({WS}*(?:Radius{WS}*\\.{WS}*circular{WS}*\\({WS}*([0-9]+(?:\\.[0-9]+)?)|([0-9]+(?:\\.[0-9]+)?))",
        ci("circular"),
        ci("all")
    )
);
re!(
    RADIUS_ZERO_RE,
    format!(
        "{B}BorderRadius(?:Directional)?{WS}*\\.{WS}*(?:{}|{}{WS}*\\({WS}*Radius{WS}*\\.{WS}*{})",
        ci("zero"),
        ci("all"),
        ci("zero")
    )
);
re!(
    RADIUS_NAMED_RE,
    format!(
        "{B}({}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}){WS}*:{WS}*Radius{WS}*\\.{WS}*(?:{}{WS}*\\({WS}*([0-9]+(?:\\.[0-9]+)?)|{})",
        ci("topLeft"),
        ci("topStart"),
        ci("topRight"),
        ci("topEnd"),
        ci("bottomRight"),
        ci("bottomEnd"),
        ci("bottomLeft"),
        ci("bottomStart"),
        ci("top"),
        ci("bottom"),
        ci("left"),
        ci("right"),
        ci("circular"),
        ci("zero")
    )
);

/// Corner radii declared in one decoration span, in
/// `[top-left, top-right, bottom-right, bottom-left]` order.
#[derive(Debug, Clone, Copy, Default)]
struct RadiusCorners {
    declared: bool,
    values: [Option<f64>; 4],
}

fn corner_index(key: &str) -> Option<usize> {
    match key {
        "topLeft" | "topStart" => Some(0),
        "topRight" | "topEnd" => Some(1),
        "bottomRight" | "bottomEnd" => Some(2),
        "bottomLeft" | "bottomStart" => Some(3),
        _ => None,
    }
}

fn parse_radius_corners(decoration: &str) -> RadiusCorners {
    let mut corners = RadiusCorners::default();
    if !RADIUS_DECL_RE.is_match(decoration) {
        return corners;
    }
    corners.declared = true;
    for caps in RADIUS_ALL_RE.captures_iter(decoration) {
        let n = num_from_caps(&caps, 1).or_else(|| num_from_caps(&caps, 2));
        if let Some(n) = n {
            corners.values = [Some(n); 4];
        }
    }
    // `BorderRadius.zero` and `BorderRadius.all(Radius.zero)` are square.
    if RADIUS_ZERO_RE.is_match(decoration) {
        corners.values = [Some(0.0); 4];
    }
    let mut named_form = false;
    for caps in RADIUS_NAMED_RE.captures_iter(decoration) {
        let key = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        // A group-2 miss is `Radius.zero`, a declared square corner.
        let n = num_from_caps(&caps, 2).unwrap_or(0.0);
        named_form = true;
        match key {
            "top" => {
                corners.values[0] = Some(n);
                corners.values[1] = Some(n);
            }
            "bottom" => {
                corners.values[2] = Some(n);
                corners.values[3] = Some(n);
            }
            "left" | "start" => {
                corners.values[0] = Some(n);
                corners.values[3] = Some(n);
            }
            "right" | "end" => {
                corners.values[1] = Some(n);
                corners.values[2] = Some(n);
            }
            other => {
                if let Some(i) = corner_index(other) {
                    corners.values[i] = Some(n);
                }
            }
        }
    }
    // `BorderRadius.only`/`.vertical`/`.horizontal` leave the corners they do
    // not name at `Radius.zero`, so those are square, not unknown.
    if named_form {
        for value in corners.values.iter_mut() {
            if value.is_none() {
                *value = Some(0.0);
            }
        }
    }
    corners
}

/// Whether a stripe on `side` sits on a card rounded *away* from it — the same
/// condition the web engines' `side_accent_needs_a_rounded_card_in_every_text_producer`
/// pins: a left stripe needs its right corners rounded, a right stripe its
/// left corners. An unresolvable declaration counts as rounded.
fn rounded_away_from_side(corners: RadiusCorners, side: &str) -> bool {
    if !corners.declared {
        return false;
    }
    let (a, b) = if matches!(side, "right" | "end") {
        (corners.values[0], corners.values[3])
    } else {
        (corners.values[1], corners.values[2])
    };
    match (a, b) {
        (Some(a), Some(b)) => a >= SIDE_ACCENT_MIN_RADIUS_PX && b >= SIDE_ACCENT_MIN_RADIUS_PX,
        _ => true,
    }
}

// ─── The matcher site ────────────────────────────────────────────────────────

/// One match in the full source text: the variant a matcher ran on, plus the
/// match's absolute byte offset in it, so context rules can read the group
/// that owns the token instead of a line window.
struct MatchSite<'a> {
    text: &'a str,
    abs: usize,
}

/// Whether the style that owns the token is a heading: its `TextStyle` group
/// carries an explicit `fontSize` of at least 22, or the key on the line that
/// opens that group names a heading role (`headlineLarge: TextStyle(`, or a
/// wrapped `textTheme.headlineLarge?.copyWith(`). The size must live *inside*
/// the owning group, so a heading size in an adjacent widget cannot charge a
/// purple icon, and a scheme seed is never a heading.

// ─── The matchers ────────────────────────────────────────────────────────────

struct DartMatcher {
    id: &'static str,
    /// Read the view whose string literals are intact (fonts live in strings).
    in_strings: bool,
    find_all: fn(&str) -> Vec<MatchCtx>,
    test: fn(&MatchCtx, &MatchSite) -> bool,
    fmt: fn(&MatchCtx, &MatchSite) -> String,
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

fn always_true(_: &MatchCtx, _: &MatchSite) -> bool {
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
// the Dart twin of `from-purple-… to-blue-…`. The gradient and its color list
// must be the groups that own the token, so a neighboring gradient cannot
// charge a lone brand purple.
re!(HAS_COLORS_LIST_RE, format!("{B}colors{WS}*:"));

fn gradient_purple_find(l: &str) -> Vec<MatchCtx> {
    all(&NAMED_COLOR_RE, l)
}

fn gradient_purple_test(m: &MatchCtx, site: &MatchSite) -> bool {
    let name = m.g(1);
    if named_color_class(name) != "purple" {
        return false;
    }
    // The token must sit in a `colors: [...]` list whose own call is a
    // `…Gradient(` constructor. Reading the list's parent by name keeps a
    // neighboring gradient in an ancestor widget out of the verdict.
    let Some((list_open, _)) = group_bounds(site.text, site.abs) else {
        return false;
    };
    if !site.text[list_open..].starts_with('[') {
        return false;
    }
    let Some((ctor_open, ctor_close)) = group_bounds(site.text, list_open) else {
        return false;
    };
    if !opener_name(site.text, ctor_open).ends_with("Gradient") {
        return false;
    }
    let span = &site.text[ctor_open..ctor_close];
    HAS_COLORS_LIST_RE.is_match(span) && second_chromatic(span, name).is_some()
}

// ai-color-palette ─ purple on a heading: a hex literal or named purple in
// the `TextStyle`/`copyWith` group that also carries the heading cue, so the
// color of an icon that merely sits beside a heading size stays quiet.
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

re!(
    FONT_SIZE_NUM_RE,
    format!("{B}{}{WS}*:{WS}*([0-9]+(?:\\.[0-9]+)?)", ci("fontSize"))
);

/// The documented threshold: an explicit size at or above this reads as a
/// heading. Compared numerically, so 30 and 41 count like 32 does.
const HEADING_MIN_FONT_SIZE: f64 = 22.0;

fn heading_purple_hex_find(l: &str) -> Vec<MatchCtx> {
    all(&HEX_COLOR_RE, l)
}

fn heading_owned(site: &MatchSite) -> bool {
    let Some((open, close)) = group_bounds(site.text, site.abs) else {
        return false;
    };
    let line_start = site.text[..open].rfind('\n').map(|p| p + 1).unwrap_or(0);
    if HEADING_CUE_RE.is_match(&site.text[line_start..open]) {
        return true;
    }
    FONT_SIZE_NUM_RE
        .captures_iter(&site.text[open..close])
        .filter_map(|c| num_from_caps(&c, 1))
        .any(|n| n >= HEADING_MIN_FONT_SIZE)
}

fn heading_purple_hex_test(m: &MatchCtx, site: &MatchSite) -> bool {
    rgb_from_hex(m.g(1)).map(is_purple_hex).unwrap_or(false) && heading_owned(site)
}

fn heading_purple_named_find(l: &str) -> Vec<MatchCtx> {
    all(&NAMED_COLOR_RE, l)
}

fn heading_purple_named_test(m: &MatchCtx, site: &MatchSite) -> bool {
    named_color_class(m.g(1)) == "purple" && heading_owned(site)
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

fn cream_test(m: &MatchCtx, _: &MatchSite) -> bool {
    rgb_from_hex(m.g(1))
        .map(is_cream_hex)
        .unwrap_or(false)
}

// side-tab ─ a one-sided thick chromatic border on a card rounded *away* from
// the stripe: `Border(left: BorderSide(color: …, width: 4))` beside a
// `borderRadius`. The rounded-away condition, the 2px threshold on a rounded
// card, and the square-card exemption mirror the web engines' pinned
// `side_accent_needs_a_rounded_card_in_every_text_producer` behavior.
re!(
    SIDE_TAB_DART_RE,
    format!("{B}(left|right|start|end){WS}*:{WS}*BorderSide{WS}*\\(")
);
re!(
    SIDE_MEMBER_ANY_RE,
    format!(
        "{B}(left|right|start|end|top|bottom){WS}*:{WS}*(?:{}{WS}+)?BorderSide{B}",
        ci("const")
    )
);

fn side_tab_find(l: &str) -> Vec<MatchCtx> {
    all(&SIDE_TAB_DART_RE, l)
}

fn side_tab_test(m: &MatchCtx, site: &MatchSite) -> bool {
    // The border constructor that owns this member: `Border(` or
    // `BorderDirectional(` (where `start`/`end` belong in Flutter).
    let Some((border_open, border_close)) = group_bounds(site.text, site.abs) else {
        return false;
    };
    if !matches!(
        opener_name(site.text, border_open),
        "Border" | "BorderDirectional"
    ) {
        return false;
    }
    let border_span = &site.text[border_open..border_close];
    if SIDE_MEMBER_ANY_RE.find_iter(border_span).count() > 1 {
        return false; // uniform or multi-sided: not a one-sided accent
    }
    let Some((args_open, args_close)) = group_bounds(site.text, site.abs + m.whole().len()) else {
        return false;
    };
    let Some((_, width)) = parse_border_side_args(&site.text[args_open..args_close]) else {
        return false;
    };
    let Some((deco_open, deco_close)) = ancestor_group(site.text, site.abs, 2) else {
        return false;
    };
    let corners = parse_radius_corners(&site.text[deco_open..deco_close]);
    if !rounded_away_from_side(corners, m.g(1)) {
        return false; // a square card's side rule is a classic accent
    }
    width >= 2.0
}

/// Reads one `BorderSide(…)` argument body and returns `(color label, width)`.
/// `None` when the stroke is neutral. Width defaults to Flutter's implicit
/// `1.0` when the argument is absent.
fn parse_border_side_args(args: &str) -> Option<(String, f64)> {
    if let Some(d) = HEX_COLOR_RE.captures(args).and_then(|c| c.get(1)) {
        let rgb = rgb_from_hex(d.as_str())?;
        if spread(rgb) < 30.0 {
            return None; // neutral stroke — not an accent
        }
        return Some((
            format!("Color(0x{})", d.as_str()),
            border_side_width(args),
        ));
    }
    let name = NAMED_COLOR_RE.find(args).and_then(|mm| {
        let name = &mm.as_str()["Colors.".len()..];
        match named_color_class(name) {
            "neutral" => None,
            // Purple is chromatic like any other accent here; the old
            // exclusion made `Colors.purple` disagree with its own hex.
            _ => Some(name.to_string()),
        }
    })?;
    Some((format!("Colors.{name}"), border_side_width(args)))
}

fn border_side_width(args: &str) -> f64 {
    BORDER_SIDE_WIDTH_RE
        .captures(args)
        .and_then(|c| c.get(1))
        .and_then(|mm| mm.as_str().parse::<f64>().ok())
        .unwrap_or(1.0)
}

re!(BORDER_SIDE_WIDTH_RE, format!("width{WS}*:{WS}*({D}+(?:\\.[0-9]+)?)"));

// ─── The table ───────────────────────────────────────────────────────────────

const DART_MATCHERS: &[DartMatcher] = &[
    DartMatcher {
        id: "bounce-easing",
        in_strings: false,
        find_all: bounce_find,
        test: always_true,
        fmt: |m, _| m.whole().to_string(),
    },
    DartMatcher {
        id: "overused-font",
        in_strings: true,
        find_all: font_family_find,
        test: always_true,
        fmt: |m, _| m.whole().to_string(),
    },
    DartMatcher {
        id: "overused-font",
        in_strings: true,
        find_all: google_fonts_find,
        test: always_true,
        fmt: |m, _| format!("Google Fonts: {}", google_fonts_display(m.g(1))),
    },
    DartMatcher {
        id: "ai-color-palette",
        in_strings: false,
        find_all: gradient_purple_find,
        test: gradient_purple_test,
        fmt: |m, _| format!("{} gradient", m.whole()),
    },
    DartMatcher {
        id: "ai-color-palette",
        in_strings: false,
        find_all: heading_purple_hex_find,
        test: heading_purple_hex_test,
        // The token match ends inside the literal (before the closing paren);
        // rebuild the label from the digits so the snippet reads whole.
        fmt: |m, _| format!("Color(0x{}) on heading", m.g(1)),
    },
    DartMatcher {
        id: "ai-color-palette",
        in_strings: false,
        find_all: heading_purple_named_find,
        test: heading_purple_named_test,
        fmt: |m, _| format!("{} on heading", m.whole()),
    },
    DartMatcher {
        id: "cream-palette",
        in_strings: false,
        find_all: cream_find,
        test: cream_test,
        fmt: |m, _| format!("Color(0x{}) page background", m.g(1)),
    },
    DartMatcher {
        id: "side-tab",
        in_strings: false,
        find_all: side_tab_find,
        test: side_tab_test,
        fmt: |m, site| {
            let args = group_bounds(site.text, site.abs + m.whole().len())
                .map(|(open, close)| &site.text[open..close])
                .unwrap_or("");
            match parse_border_side_args(args) {
                Some((color, width)) => {
                    let side = m.g(1).to_string();
                    format!("Border({side}: BorderSide(color: {color}, width: {width}))")
                }
                None => "one-sided BorderSide".to_string(),
            }
        },
    },
];

/// The Dart pass over one source file. Runs after the shared web matcher pass
/// inside `detect_text`, so all `.dart` findings flow through that engine's
/// dedupe, waivers, and output.
pub fn dart_findings(content: &str, file_path: &str) -> Vec<Finding> {
    let stripped = strip_dart_comments(content);
    let code_only = blank_string_contents(&stripped);
    let stripped_lines: Vec<&str> = stripped.split('\n').collect();
    let code_lines: Vec<&str> = code_only.split('\n').collect();
    // Findings come out in line order, then table order inside a line, the
    // shape the text engine's other producers use.
    let mut line_start = 0usize;
    let mut findings = Vec::new();
    for (i, code_line) in code_lines.iter().enumerate() {
        for matcher in DART_MATCHERS {
            let text = if matcher.in_strings {
                stripped.as_str()
            } else {
                code_only.as_str()
            };
            let line = if matcher.in_strings {
                stripped_lines[i]
            } else {
                *code_line
            };
            for m in (matcher.find_all)(line) {
                let site = MatchSite {
                    text,
                    abs: line_start + m.index,
                };
                if (matcher.test)(&m, &site) {
                    findings.push(finding(
                        matcher.id,
                        file_path,
                        &(matcher.fmt)(&m, &site),
                        (i + 1) as f64,
                    ));
                }
            }
        }
        line_start += code_line.len() + 1; // the '\n' we split on
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
    fn nested_block_comments_stay_commented() {
        let out = strip_dart_comments("/* outer /* inner */ fontFamily: 'Inter' */\n");
        assert!(!out.contains("fontFamily"));
        assert!(!has_id(
            "/* outer /* inner */ fontFamily: 'Inter' */\nWidget b() => const SizedBox();\n",
            "overused-font"
        ));
    }

    #[test]
    fn raw_strings_do_not_open_interpolation() {
        // `r'${'` is literal text; it must not swallow the comment below.
        let code = "final marker = r'${';\n// fontFamily: 'Inter'\nWidget b() => const SizedBox();\n";
        let out = strip_dart_comments(code);
        assert!(!out.contains("fontFamily"));
        assert!(!has_id(code, "overused-font"));
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
    fn tell_tokens_inside_string_literals_stay_quiet() {
        // UI copy may name a banned curve; copy is not code.
        assert!(!has_id(
            "Widget b() => const Text('Curves.bounceOut is banned');",
            "bounce-easing"
        ));
        assert!(!has_id(
            "Widget b() => const Text('fontFamily: Inter');",
            "overused-font"
        ));
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
        // Two purple families in one gradient are the same tell.
        assert!(has_id(
            "gradient: LinearGradient(colors: [Colors.deepPurple, Colors.indigo]),",
            "ai-color-palette"
        ));
        // A lone named purple is a legitimate brand accent.
        assert!(!has_id("color: Colors.deepPurple,", "ai-color-palette"));
        // A gradient with no other chromatic member is clean.
        assert!(!has_id(
            "gradient: LinearGradient(colors: [Colors.teal, Colors.blue]),",
            "ai-color-palette"
        ));
        assert!(!has_id(
            "gradient: LinearGradient(colors: [Colors.deepPurple, Colors.black26]),",
            "ai-color-palette"
        ));
        // A neighboring gradient in the same widget must not charge it.
        assert!(!has_id(
            r#"Column(children: [
                 const Icon(Icons.info, color: Colors.deepPurple),
                 DecoratedBox(decoration: BoxDecoration(gradient: LinearGradient(colors: [Colors.teal, Colors.blue]))),
               ])"#,
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
        // Every size at or above the documented 22 counts.
        for size in ["22", "29", "30", "31", "32", "40", "41", "48"] {
            assert!(
                has_id(
                    &format!("style: TextStyle(fontSize: {size}, color: Colors.deepPurple),"),
                    "ai-color-palette"
                ),
                "fontSize {size} should count as a heading"
            );
        }
        // Small text or body label contexts are not headings.
        assert!(!has_id(
            "bodyText: TextStyle(fontSize: 14, color: Colors.deepPurple),",
            "ai-color-palette"
        ));
        assert!(!has_id(
            "bodyText: TextStyle(fontSize: 21, color: Colors.deepPurple),",
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
        // An unrelated heading size beside a purple icon is not a heading.
        assert!(!has_id(
            r#"Column(children: [
                 const Text('Title', style: TextStyle(fontSize: 32)),
                 const Icon(Icons.info, color: Colors.deepPurple),
               ])"#,
            "ai-color-palette"
        ));
        // Seeding a scheme with a brand purple is a palette choice.
        assert!(!has_id(
            r#"final scheme = ColorScheme.fromSeed(seedColor: const Color(0xff7952b3));
               const headingStyle = TextStyle(fontSize: 24);"#,
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
        assert!(!has_id("backgroundColor: Colors.white,", "cream-palette"));
    }

    #[test]
    fn one_sided_border_flags_only_on_a_card_rounded_away_from_it() {
        // The tell: a stripe on a rounded card.
        assert!(has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: const Border(left: BorderSide(color: Color(0xFF8B5CF6), width: 4)),
               )"#,
            "side-tab"
        ));
        // Rounded away regardless of how far the radius sits from the border.
        assert!(has_id(
            r#"BoxDecoration(
                 color: Colors.white,
                 borderRadius: BorderRadius.circular(12),
                 boxShadow: [BoxShadow(color: Colors.black12, blurRadius: 12)],
                 border: const Border(left: BorderSide(color: Colors.teal, width: 4)),
               )"#,
            "side-tab"
        ));
        // Flutter puts `start`/`end` on BorderDirectional.
        assert!(has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(10),
                 border: const BorderDirectional(start: BorderSide(color: Colors.teal, width: 6)),
               )"#,
            "side-tab"
        ));
        // A rounded card takes a 2px stripe, the web engine's lower threshold.
        assert!(has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: const Border(left: BorderSide(color: Colors.teal, width: 2)),
               )"#,
            "side-tab"
        ));
        // Named purple agrees with its hex equivalent.
        assert!(has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: const Border(left: BorderSide(color: Colors.purple, width: 4)),
               )"#,
            "side-tab"
        ));
        // Square cards are a classic side rule, not the tell.
        assert!(!has_id(
            "border: const Border(left: BorderSide(color: Colors.teal, width: 4)),",
            "side-tab"
        ));
        // Rounded *toward* the stripe is not the tell either.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.only(topLeft: Radius.circular(12), bottomLeft: Radius.circular(12)),
                 border: const Border(left: BorderSide(color: Colors.teal, width: 4)),
               )"#,
            "side-tab"
        ));
        // A declared zero radius is square, not unknown.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.zero,
                 border: const Border(left: BorderSide(color: Colors.teal, width: 4)),
               )"#,
            "side-tab"
        ));
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.only(topLeft: Radius.zero, topRight: Radius.zero, bottomRight: Radius.circular(12), bottomLeft: Radius.zero),
                 border: const Border(left: BorderSide(color: Colors.teal, width: 4)),
               )"#,
            "side-tab"
        ));
        // Corners under the rounded-card threshold stay square.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(2),
                 border: const Border(left: BorderSide(color: Colors.teal, width: 4)),
               )"#,
            "side-tab"
        ));
        // Uniform borders are fine even when chromatic.
        assert!(!has_id(
            "border: Border.all(color: Color(0xFF1B4B5A), width: 4),",
            "side-tab"
        ));
        // Thin strokes are fine.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: const Border(left: BorderSide(color: Colors.teal, width: 1)),
               )"#,
            "side-tab"
        ));
        // Neutral stroke is fine.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: const Border(left: BorderSide(color: Colors.grey, width: 4)),
               )"#,
            "side-tab"
        ));
        // Two-sided border is not a side accent.
        assert!(!has_id(
            r#"BoxDecoration(
                 borderRadius: BorderRadius.circular(12),
                 border: Border(
                   left: BorderSide(color: Colors.teal, width: 4),
                   right: BorderSide(color: Colors.teal, width: 4),
                 ),
               )"#,
            "side-tab"
        ));
    }
}

