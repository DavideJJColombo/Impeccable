//! `side-tab` on a status row or cell: a coloured stripe down one side of a
//! table row or cell, drawn as an inset box-shadow (`inset 3px 0 0 red`) or
//! a side border (`border-left: 3px solid red`).
//!
//! The card scans in `css_scan` skip every selector with a table tag and
//! report a stripe only on a card rounded away from it, so a stripe marking
//! a broken or new row went unreported. A row or cell is never a rounded
//! card, so neither gate fits it. It reports here instead when the rule is
//! about the row's state (an `is-*` / `has-*` class, a BEM modifier, a
//! `data-*` attribute) or paints nothing but the stripe. A selection or
//! hover indicator (`:hover`, `.is-selected`, `[aria-current]`) stays silent,
//! as it does on a card.
//!
//! Colours read through `var()`: the rule's own sheet first, then whatever
//! tokens the caller collected across the project, then a token name that
//! says the colour (`--danger`, `--accent-red`). A token that resolves
//! neutral or transparent, or does not resolve and names no colour, is not
//! a stripe.

use crate::checks::css_scan::{
    collect_css_custom_props, css_length_to_px, parse_css_decl_block, selector_is_state_indicator,
    CustomProps, PatternFinding, CSS_RULE_BLOCK_SOURCE,
};
use crate::checks::measures::resolve_var_refs;
use crate::checks::rules::{extract_shadow_lengths, find_shadow_color};
use crate::color::{parse_any_color, rgb_to_oklch_lc, Rgba};
use crate::js::{self, number_to_string};
use crate::js_ext_a::split_commas_outside_parens;
use once_cell::sync::Lazy;
use regex::Regex;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: Lazy<Regex> = Lazy::new(|| Regex::new(&$pat).expect(stringify!($name)));
    };
}

re!(RULE_RE, CSS_RULE_BLOCK_SOURCE.to_string());
re!(COMMENT_RE, r"(?s)/\*.*?\*/".to_string());
re!(
    ROW_TAG_RE,
    r"(?i)(?:^|[^-_A-Za-z0-9.#\[:])(?:tr|td|th)(?:$|[^-_A-Za-z0-9])".to_string()
);
re!(
    ROW_ROLE_RE,
    r#"(?i)\[role\s*=\s*["']?(?:row|cell|gridcell|rowheader)["']?\s*\]"#.to_string()
);
re!(
    ROW_CLASS_RE,
    r"(?i)\.(?:[A-Za-z0-9]+[-_]+)*(?:row|cell|gridcell)(?:[-_]+[A-Za-z0-9]+)*(?:$|[^-_A-Za-z0-9])"
        .to_string()
);
re!(
    STATUS_MARKER_RE,
    r"(?i)\.(?:is|has)-[-_A-Za-z0-9]|\.[A-Za-z0-9_-]+--[A-Za-z0-9]|\[\s*(?:data-[-_A-Za-z0-9]+|aria-invalid)"
        .to_string()
);
re!(INSET_WORD_RE, r"(?i)\binset\b".to_string());
re!(
    VAR_NAME_RE,
    r"(?i)var\(\s*(--[-_A-Za-z0-9]+)\s*(?:,[^)]*)?\)".to_string()
);
// A token named for a colour or a status reads as that colour when no
// declaration of it is in reach.
re!(
    COLOUR_TOKEN_NAME_RE,
    format!(
        "(?i)(?:^|-)(?:{})(?:-|$)",
        [
            "accent", "red", "orange", "amber", "yellow", "lime", "green", "emerald", "teal",
            "cyan", "blue", "indigo", "violet", "purple", "magenta", "pink", "rose", "coral",
            "crimson", "scarlet", "danger", "error", "warning", "warn", "success", "critical",
            "negative", "positive", "alert",
        ]
        .join("|")
    )
);
re!(
    NO_PAINT_RE,
    r"(?i)^(?:none|transparent|inherit|initial|unset|revert|currentcolor)$".to_string()
);
re!(
    BORDER_STYLE_NONE_RE,
    r"(?i)^(?:none|hidden)$".to_string()
);
re!(
    BORDER_STYLE_RE,
    r"(?i)^(?:solid|dashed|dotted|double|groove|ridge|inset|outset)$".to_string()
);

/// Stripe thickness, in px, read as a row or cell marker.
const MIN_PX: f64 = 2.0;
const MAX_PX: f64 = 6.0;

/// The side borders a stripe can be drawn with, and the edge each paints.
const SIDE_BORDERS: &[(&str, &str)] = &[
    ("border-left", "left"),
    ("border-right", "right"),
    ("border-inline-start", "left"),
    ("border-inline-end", "right"),
];

/// Whether a selector's subject (the compound after its last combinator) is
/// a table row or cell: a `tr` / `td` / `th`, an ARIA row or cell, or a
/// class named for one (`.row`, `.data-cell`, `.table__row`). A selector
/// list is one when any of its selectors is.
pub fn is_row_or_cell_selector(selector: &str) -> bool {
    split_commas_outside_parens(selector).into_iter().any(|part| {
        let subject = subject_compound(js::trim(part));
        ROW_TAG_RE.is_match(subject) || ROW_ROLE_RE.is_match(subject) || ROW_CLASS_RE.is_match(subject)
    })
}

/// Whether a selector styles a state of the row rather than every row: an
/// `is-*` / `has-*` class, a BEM modifier, a `data-*` attribute or
/// `[aria-invalid]` anywhere in it.
pub fn selector_has_status_marker(selector: &str) -> bool {
    STATUS_MARKER_RE.is_match(selector)
}

fn subject_compound(selector: &str) -> &str {
    let b = selector.as_bytes();
    let mut depth = 0i32;
    let mut p = b.len();
    while p > 0 {
        match b[p - 1] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => depth -= 1,
            b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'+' | b'~' if depth <= 0 => break,
            _ => {}
        }
        p -= 1;
    }
    &selector[p..]
}

/// Whether a declaration paints something of its own beside the stripe.
fn paints(prop: &str) -> bool {
    (prop.starts_with("background") && prop != "background-clip")
        || (prop.starts_with("border")
            && !matches!(prop, "border-collapse" | "border-spacing")
            && !prop.contains("radius"))
        || prop.starts_with("outline")
        || prop.starts_with("text-decoration")
        || matches!(prop, "color" | "opacity" | "filter" | "fill" | "stroke")
        || prop == "box-shadow"
}

/// What a colour value paints: `Some(true)` an accent, `Some(false)` a
/// neutral or nothing, `None` unknown (an unresolved token with no colour in
/// its name).
fn accent_colour(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<bool> {
    let resolved = resolve_var_refs(value, &|name: &str| lookup(name), 0);
    let resolved = js::trim(&resolved);
    if resolved.is_empty() || NO_PAINT_RE.is_match(resolved) {
        return Some(false);
    }
    if let Some(caps) = VAR_NAME_RE.captures(resolved) {
        return COLOUR_TOKEN_NAME_RE.is_match(&caps[1]).then_some(true);
    }
    let colour = parse_any_color(Some(resolved))
        .or_else(|| find_shadow_color(resolved).and_then(|c| c.color));
    Some(colour.is_some_and(|c| is_accent(&c)))
}

/// OKLCH chroma a row stripe needs: a status colour (red, green, amber, a
/// brand blue) sits well above it, a slate divider (`#2e3d54`, about 0.04)
/// below, though its channels spread past the card scans' 30.
pub const MIN_ROW_STRIPE_CHROMA: f64 = 0.07;

/// Whether a colour reads as an accent on a row: visible, and coloured
/// rather than a tinted grey.
pub fn is_row_stripe_accent(c: &Rgba) -> bool {
    c.alpha_or_one() >= 0.1
        && js::math_max3(c.r, c.g, c.b) - js::math_min3(c.r, c.g, c.b) >= 30.0
        && rgb_to_oklch_lc(c).1 >= MIN_ROW_STRIPE_CHROMA
}

fn is_accent(c: &Rgba) -> bool {
    is_row_stripe_accent(c)
}

fn in_band(px: f64) -> bool {
    (MIN_PX..=MAX_PX).contains(&px)
}

/// An inset box-shadow layer that is a stripe down the left or right edge:
/// `(thickness, edge)`.
fn inset_stripe(
    value: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(f64, &'static str)> {
    for layer in split_commas_outside_parens(value) {
        let layer = js::trim(layer);
        if !INSET_WORD_RE.is_match(layer) {
            continue;
        }
        let layer = resolve_var_refs(layer, &|name: &str| lookup(name), 0);
        let (colour, lengths) = match VAR_NAME_RE.find(&layer) {
            Some(var) => (
                accent_colour(var.as_str(), lookup),
                extract_shadow_lengths(&layer, Some((var.start(), var.end()))),
            ),
            None => match find_shadow_color(&layer) {
                Some(info) => (
                    Some(info.color.is_some_and(|c| is_accent(&c))),
                    extract_shadow_lengths(&layer, Some((info.start, info.end))),
                ),
                None => continue,
            },
        };
        if colour != Some(true) || lengths.len() < 2 {
            continue;
        }
        let at = |i: usize| lengths.get(i).copied().filter(|v| v.is_finite()).unwrap_or(0.0);
        let (x, y, blur, spread) = (at(0), at(1), at(2), at(3));
        if y != 0.0 || blur != 0.0 || spread != 0.0 || !in_band(x.abs()) {
            continue;
        }
        return Some((x.abs(), if x > 0.0 { "left" } else { "right" }));
    }
    None
}

/// A side border that is a stripe: `(property, thickness, edge)`.
fn border_stripe(
    decls: &crate::checks::css_scan::DeclMap,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<(&'static str, f64, &'static str)> {
    // A border all round is a frame, not a stripe.
    for all_round in ["border", "border-width", "border-color"] {
        if decls.get(all_round).is_some_and(|v| !matches!(js::trim(v), "0" | "none" | "0px")) {
            return None;
        }
    }
    for &(prop, edge) in SIDE_BORDERS {
        let shorthand = decls.get(prop).map(String::as_str).unwrap_or("");
        let mut width = None;
        let mut colour = String::new();
        let mut styled = shorthand.is_empty();
        let resolved = resolve_var_refs(shorthand, &|name: &str| lookup(name), 0);
        for token in split_ws_outside_parens(&resolved) {
            if BORDER_STYLE_NONE_RE.is_match(token) {
                styled = false;
                width = None;
                break;
            }
            if BORDER_STYLE_RE.is_match(token) {
                styled = true;
            } else if let Some(px) = css_length_to_px(token) {
                width = Some(px);
            } else {
                colour = token.to_string();
            }
        }
        if let Some(w) = decls.get(&format!("{prop}-width")) {
            width = css_length_to_px(&resolve_var_refs(w, &|name: &str| lookup(name), 0));
        }
        if let Some(c) = decls.get(&format!("{prop}-color")) {
            colour = c.clone();
        }
        if let Some(s) = decls.get(&format!("{prop}-style")) {
            styled = !BORDER_STYLE_NONE_RE.is_match(js::trim(s));
        }
        let Some(width) = width.filter(|w| in_band(*w)) else {
            continue;
        };
        if !styled || colour.is_empty() || accent_colour(&colour, lookup) != Some(true) {
            continue;
        }
        return Some((prop, width, edge));
    }
    None
}

fn split_ws_outside_parens(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c.is_whitespace() && depth <= 0 {
            if let Some(s) = start.take() {
                out.push(&value[s..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&value[s..]);
    }
    out
}

/// Every rule in `content` that draws a side stripe on a status row or cell.
/// `tokens` are custom properties declared elsewhere in the project; the
/// sheet's own declarations win over them. `index` is the byte offset of the
/// rule's selector in `content`.
pub fn scan_css_text_for_row_stripe(content: &str, tokens: Option<&CustomProps>) -> Vec<PatternFinding> {
    if !content.contains("inset") && !content.contains("border-") {
        return Vec::new();
    }
    // Comments blanked byte for byte, so offsets index `content`.
    let text = COMMENT_RE.replace_all(content, |m: &regex::Captures| {
        m[0].bytes().map(|b| if b == b'\n' { '\n' } else { ' ' }).collect::<String>()
    });
    let own = collect_css_custom_props(&text);
    let lookup = |name: &str| -> Option<String> {
        own.get(name).or_else(|| tokens.and_then(|t| t.get(name))).cloned()
    };
    let mut findings = Vec::new();
    for m in RULE_RE.captures_iter(&text) {
        let raw = m.get(1).unwrap();
        let selector = js::trim(raw.as_str());
        if selector.is_empty() || selector.starts_with('@') || !is_row_or_cell_selector(selector) {
            continue;
        }
        if selector_is_state_indicator(selector) {
            continue;
        }
        let decls = parse_css_decl_block(&m[2]);
        if decls
            .get("width")
            .and_then(|w| css_length_to_px(&resolve_var_refs(w, &lookup, 0)))
            .is_some_and(|w| w <= 40.0)
        {
            continue;
        }
        // A rounded box is a card, and the card scans read it.
        if decls.iter().any(|(p, v)| {
            p.contains("radius") && css_length_to_px(js::trim(v)).map_or(true, |r| r > 0.0)
        }) {
            continue;
        }
        let (stripe_prop, snippet) = if let Some((px, edge)) = decls
            .get("box-shadow")
            .and_then(|v| inset_stripe(v, &lookup))
        {
            (
                "box-shadow",
                format!("{} — inset box-shadow {}px stripe on a row or cell ({edge} edge)", collapse(selector), number_to_string(px)),
            )
        } else if let Some((prop, px, edge)) = border_stripe(&decls, &lookup) {
            (
                prop,
                format!("{} — {prop} {}px stripe on a row or cell ({edge} edge)", collapse(selector), number_to_string(px)),
            )
        } else {
            continue;
        };
        let stripe_only = decls
            .iter()
            .all(|(p, _)| !paints(p) || p == stripe_prop || p.starts_with(&format!("{stripe_prop}-")));
        if !selector_has_status_marker(selector) && !stripe_only {
            continue;
        }
        let leading = raw.as_str().len() - js::trim_start(raw.as_str()).len();
        findings.push(PatternFinding {
            id: "side-tab".to_string(),
            snippet,
            selector: Some(collapse(selector)),
            index: Some(raw.start() + leading),
            severity: None,
        });
    }
    findings
}

fn collapse(selector: &str) -> String {
    selector.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippets(css: &str) -> Vec<String> {
        scan_css_text_for_row_stripe(css, None).into_iter().map(|f| f.snippet).collect()
    }

    #[test]
    fn a_stripe_on_a_status_row_or_cell_is_a_side_tab() {
        for css in [
            "tr.is-broken > td:first-child { box-shadow: inset 3px 0 0 #d0342c; }",
            ".cell { box-shadow: inset 3px 0 0 red; }",
            "tr[data-state=failed] td:first-child { border-left: 3px solid #d0342c; }",
            ".grid [role=row].has-error { box-shadow: inset -4px 0 0 oklch(63% .23 27); }",
            ".table__row--new { border-inline-start: 2px solid #16a34a; padding: 4px; }",
            ":root { --mark: #d0342c; } tr.is-x td:first-child { box-shadow: inset 3px 0 0 var(--mark); }",
            "tr.is-x td { box-shadow: inset 3px 0 0 var(--color-danger); }",
        ] {
            assert_eq!(snippets(css).len(), 1, "{css}");
        }
        assert_eq!(
            snippets("tr.is-broken > td:first-child { box-shadow: inset 3px 0 0 #d0342c; }"),
            vec!["tr.is-broken > td:first-child — inset box-shadow 3px stripe on a row or cell (left edge)"]
        );
    }

    #[test]
    fn rows_and_cells_that_are_not_a_side_tab_stay_silent() {
        for css in [
            // Selection and hover indicators.
            "tr.is-selected td:first-child { box-shadow: inset 3px 0 0 #2563eb; }",
            "tr:hover td:first-child { box-shadow: inset 3px 0 0 #2563eb; }",
            "tr[aria-current] td { box-shadow: inset 3px 0 0 #2563eb; }",
            // Neutral, transparent, faint or unresolved-and-unnamed colours.
            "tr.is-x td { box-shadow: inset 3px 0 0 #e5e7eb; }",
            ".vital-cell { border-left: 2px solid #2e3d54; padding-left: 18px; }",
            "tr.is-x td { box-shadow: inset 3px 0 0 transparent; }",
            "tr.is-x td { box-shadow: inset 3px 0 0 rgba(220, 38, 38, .05); }",
            "tr.is-x td { box-shadow: inset 3px 0 0 var(--line); }",
            ":root { --line: #ddd; } tr.is-x td { box-shadow: inset 3px 0 0 var(--line); }",
            // A divider, a sticky-column shadow, a header underline, a glow.
            "td + td { box-shadow: inset 1px 0 0 #d0342c; }",
            "td.is-sticky { box-shadow: inset -8px 0 8px -8px rgba(0,0,0,.2); }",
            "th.is-sorted { box-shadow: inset 0 -3px 0 #2563eb; }",
            "tr.is-x td { box-shadow: inset 3px 0 6px #d0342c; }",
            // A plain cell that paints more than the stripe, a frame all round,
            // a table, a card.
            "td { box-shadow: inset 3px 0 0 #d0342c; background: #fff; }",
            "td.is-x { border: 3px solid #d0342c; }",
            "table.is-x { box-shadow: inset 3px 0 0 #d0342c; }",
            ".card.is-x { box-shadow: inset 3px 0 0 #d0342c; }",
            // A rounded row-like card is the card scans' to read.
            ".row-card.is-x { box-shadow: inset 3px 0 0 #d0342c; border-radius: 8px; }",
            // A thick band is not a stripe.
            "tr.is-x td { border-left: 12px solid #d0342c; }",
            "/* tr.is-x td { box-shadow: inset 3px 0 0 #d0342c; } */",
        ] {
            assert_eq!(snippets(css), Vec::<String>::new(), "{css}");
        }
    }

    #[test]
    fn project_tokens_resolve_a_colour_the_sheet_does_not_declare() {
        let css = "tr.is-x td:first-child { box-shadow: inset 3px 0 0 var(--pr-mark); }";
        assert!(scan_css_text_for_row_stripe(css, None).is_empty());
        let mut tokens = CustomProps::new();
        tokens.set("--pr-mark", "oklch(63% .23 27)".to_string());
        assert_eq!(scan_css_text_for_row_stripe(css, Some(&tokens)).len(), 1);
        tokens.set("--pr-line", "#e5e7eb".to_string());
        let neutral = "tr.is-x td { box-shadow: inset 3px 0 0 var(--pr-line); }";
        assert!(scan_css_text_for_row_stripe(neutral, Some(&tokens)).is_empty());
        // The sheet's own declaration wins over the project's.
        let own = format!(":root {{ --pr-mark: #ccc; }} {css}");
        assert!(scan_css_text_for_row_stripe(&own, Some(&tokens)).is_empty());
    }
}
