# Flutter / Dart support

`impeccable detect` and the design hook read Flutter widget code. The engine treats `.dart` sources as UI, the same way it treats `.tsx` or `.svelte`, and flags the design tells it finds there with the existing rule vocabulary. This page explains the data path and where it stops.

## The path a `.dart` file takes

Three steps, all in the one `impeccable` binary:

1. **Walk.** `impeccable detect lib/` collects files by extension; `.dart` joined the built-in list, so no `detector.extensions` config is needed.
2. **Route.** `.dart` is not HTML, so the file goes to the regex (text) engine with every other non-HTML source. The web matcher pass still runs first and stays quiet on a Dart line: its triggers are web tokens (`border-left: 4px solid #f00`, Tailwind classes) that Dart widget code never carries. Then the Dart pass runs.
3. **Dart pass** (`crates/detect/src/dart.rs`). Two moving parts:
   - **Comment and string stripping.** A state machine blanks `//` and `/* */` comments, replacing their bytes with spaces so line and column numbers survive, while string literals stay intact: `'https://example.com/x'` inside a string is a URL, not a comment opener. `${…}` interpolation is tracked, so a quote inside interpolation does not end the string.
   - **Matchers.** A per-line regex table with a small surrounding-line window per match. Every row targets an existing registry id, so rule names, categories, severities, and descriptions come from the same registry the web engines use; only the trigger syntax differs:

   | Rule id | Web trigger | Dart trigger |
   |---|---|---|
   | `bounce-easing` | `cubic-bezier(…, y > 1.1)` | `Curves.bounceOut`, `elasticIn/Out/InOut`, `easeOutBack` |
   | `overused-font` | `font-family: Inter` | `fontFamily: 'Inter'`, `GoogleFonts.inter()` |
   | `ai-color-palette` | `from-purple-500 to-blue-500` | purple gradient lists; purple on heading-size text |
   | `cream-palette` | cream page background | warm off-white `scaffoldBackgroundColor: Color(0xFDF6EC)` |
   | `side-tab` | `border-left: 4px solid` | `Border(left: BorderSide(chromatic, width ≥ 3))` on a non-rounded container |

   Colors parse from Flutter's `Color(0xAARRGGBB)` layout (alpha first, the opposite order of web hex) and are judged by hue and saturation, the way the web engine judges `#9c27b0` against `#2196f3`.

After the pass, findings flow through the text engine's shared machinery: the same dedupe, the same advisory tiering (`Curves.bounceOut` is advisory, not an error), and the same inline waivers. A `// impeccable-disable <rule>: <reason>` line a Dart file works exactly like the HTML `<!-- impeccable-disable … -->` marker, because the waiver parser is comment-syntax agnostic.

## The design hook

`npx impeccable install` writes provider-native hook manifests that scan direct UI file edits. `.dart` is in both extension lists (`crates/hook/src/hook_lib.rs`), so an edit to a widget file in Claude Code, Cursor, Codex, or Grok Build surfaces Dart findings the same way it does for a `.tsx` edit.

## What stays target-independent, what does not

The detector works on widget source, and widget source is platform-blind: the same `Colors`, `Curves`, and `BorderSide` syntax ships to Android, iOS, macOS, Linux, and Windows from one `lib/`. Nothing in the Dart pass switches on the target platform. What changes per platform is the judgment layer, which reads the skills' platform references (`android.md` Material 3 rulebook, `ios.md` conventions): an iOS-shaped bottom nav on an Android build is an LLM audit finding, not a detector one.

## Known limits

- **Geometry rules still need rendering.** Nested cards, touch targets, the type scale, and contrast pairs are judged from lines, not pixels, so they remain the LLM's job on every target (or `flutter test` widget goldens).
- **Scanning a Flutter Web build by URL mostly returns nothing:** Flutter web renders into a canvas, so the browser-DOM engine sees a near-empty page. Scan the source instead.
- **Generated `*.g.dart` files** carry upstream decisions. Add a `**/*.g.dart` entry under `detector.ignoreFiles` in `.impeccable/config.json` instead of waiving each file.
- **Roboto is not flagged on purpose.** It is the Android system face; an explicit Roboto choice in Dart is a default, not a drift tell. The LLM guidance in `android.md` covers when to move off it.
- **Desktop-specific conventions** (macOS window chrome, Linux theming) have no platform reference yet; the general native guidance covers them today.
