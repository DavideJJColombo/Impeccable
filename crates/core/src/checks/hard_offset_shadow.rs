//! Advisory geometry check, shared by static HTML and rendered DOM scans.
use crate::checks::rules::RuleHit;
use crate::color::{parse_any_color, split_top_level_commas, Rgba, COLOR_FUNCTION_NAMES};
use cssparser::{Parser, ParserInput, Token};

/// Four CSS pixels separates the fixture's deliberate displaced slabs from
/// its small offset accents. This is a review threshold, not a design standard.
/// Negative spread and unresolved lengths/colors stay outside this first rule.
pub fn check_hard_offset_shadow(shadow: &str, current_color: &str, opacity: f64) -> Vec<RuleHit> {
    for layer in split_top_level_commas(shadow) {
        let Some((v, color)) = parse_layer(&layer, current_color) else {
            continue;
        };
        if !opacity.is_finite() || color.alpha_or_one() * opacity <= 0.1 {
            continue;
        }
        if !(2..=4).contains(&v.len()) {
            continue;
        }
        let blur = v.get(2).copied().unwrap_or(0.0);
        let spread = v.get(3).copied().unwrap_or(0.0);
        if blur == 0.0 && spread >= 0.0 && v[0].abs().max(v[1].abs()) >= 4.0 {
            return vec![RuleHit::new("hard-offset-shadow", format!(
                "box-shadow: {} — hard offset with no blur; check whether this belongs to the intended visual direction", layer.trim()
            ))];
        }
    }
    Vec::new()
}

// The static CSS serializer can omit whitespace between tokens (0#333 or
// rgb(0,0,0)4px). Tokenize CSS rather than splitting on spaces, keeping nested
// color functions whole and rejecting unresolved lengths and unknown paint.
fn parse_layer(layer: &str, current_color: &str) -> Option<(Vec<f64>, Rgba)> {
    let mut input = ParserInput::new(layer);
    let mut parser = Parser::new(&mut input);
    let mut lengths = Vec::new();
    let mut color = None;
    while !parser.is_exhausted() {
        parser.skip_whitespace();
        let start = parser.position();
        let token = parser.next().ok()?.clone();
        let paint = match token {
            Token::Dimension { value, unit, .. }
                if unit.eq_ignore_ascii_case("px") && value.is_finite() =>
            {
                // Keep the f64 threshold precision used by this check; the
                // tokenizer stores numeric values as f32.
                let raw = parser.slice_from(start);
                let value = raw[..raw.len() - unit.len()].parse::<f64>().ok()?;
                if !value.is_finite() {
                    return None;
                }
                lengths.push(value);
                continue;
            }
            Token::Number { .. } if parser.slice_from(start).parse::<f64>().ok()? == 0.0 => {
                lengths.push(0.0);
                continue;
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("currentcolor") => current_color,
            Token::Ident(_) | Token::Hash(_) | Token::IDHash(_) => parser.slice_from(start),
            Token::Function(ref name)
                if COLOR_FUNCTION_NAMES
                    .iter()
                    .any(|f| name.eq_ignore_ascii_case(f)) =>
            {
                parser
                    .parse_nested_block(|nested| {
                        while nested.next_including_whitespace_and_comments().is_ok() {}
                        Ok::<_, cssparser::ParseError<'_, ()>>(())
                    })
                    .ok()?;
                parser.slice_from(start)
            }
            _ => return None,
        };
        // A shadow has at most one paint; transparent/unknown paint stays out.
        let parsed = parse_any_color(Some(paint))?;
        if color.replace(parsed).is_some() {
            return None;
        }
    }
    Some((
        lengths,
        color.or_else(|| parse_any_color(Some(current_color)))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_and_functional_shadow_colors() {
        for shadow in [
            "4px 4px red",
            "4px 4px 0#333",
            "rgb(255,0,0)4px 4px",
            "gray -4px 4px",
            "4px 4px ReBeccAPurple",
            "4px 4px hwb(0 0% 0%)",
            "hwb(0 0% 0% / 50%) 4px 4px",
            "4px 4px color(srgb 1 0 0)",
            "4px 4px color-mix(in srgb, rgb(255, 0, 0) 50%, blue)",
            "4px 4px currentColor",
            "4px 4px",
        ] {
            assert_eq!(
                check_hard_offset_shadow(shadow, "red", 1.0).len(),
                1,
                "{shadow}"
            );
        }
        for shadow in [
            "4px 4px hwb(0 0% 0% / 10%)",
            "4px 4px TRANSPARENT",
            "4px 4px red blue",
            "red 4px blue 4px",
            "4px 4px notacolor",
            "4px 4px var(--color, red)",
            "4px 4px var(--color, rgb(255,0,0))",
            "4px 4px red)",
            "4px 4px hwb(0 0% 0%",
            "inset 4px 4px red",
        ] {
            assert!(
                check_hard_offset_shadow(shadow, "red", 1.0).is_empty(),
                "{shadow}"
            );
        }
        assert!(check_hard_offset_shadow("4px 4px hwb(0 0% 0% / 50%)", "red", 0.2).is_empty());
    }

    #[test]
    fn resolved_layers_and_boundaries() {
        for s in [
            "4px 4px 0 #333",
            "-6px 6px #333",
            "0 4px 0 black",
            "4px 4px",
            "4px 4px currentColor",
            "0 0 0 transparent, 4px 4px #333",
        ] {
            assert_eq!(check_hard_offset_shadow(s, "#333", 1.0).len(), 1, "{s}");
        }
        for s in [
            "3.99px 3.99px #333",
            "3.99999999px 3.99999999px red",
            "4px 4px 0.1px #333",
            "inset 4px 4px 0 #333",
            "0 0 0 4px #333",
            "4px 4px 0 transparent",
            "4px 4px rgba(0,0,0,.1)",
            "4px 4px 0 -8px #333",
            "4em 4em #333",
            "var(--shadow, 4px 4px #333)",
            "calc(2px + 2px) 4px #333",
            "4px 4px color(unknown 0 0 0)",
            "none",
        ] {
            assert!(check_hard_offset_shadow(s, "#333", 1.0).is_empty(), "{s}");
        }
        assert!(check_hard_offset_shadow("4px 4px #333", "#333", 0.0).is_empty());
        assert!(check_hard_offset_shadow("4px 4px", "transparent", 1.0).is_empty());
    }
}
