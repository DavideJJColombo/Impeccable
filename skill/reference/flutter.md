# Flutter (Dart)

For Flutter apps: Dart widget code shipping to Android, iOS, and the web. Flutter is a *native* platform app for this skill's purposes. On Android it obeys [android.md](android.md)'s Material 3 rulebook; on iOS, [ios.md](ios.md). This reference carries the Dart-specific tells those references leave unsaid and describes what the deterministic detector now sees in `.dart` files.

## The Dart slop test

A Flutter app's most common tell is default-ness: screens that read as if an AI was told "make it pretty" and answered with the Material demo catalog. The second tell is web slop ported to Dart, because that is where the model trained. `Curves.bounceOut` sits exactly where a web build would have written an overshoot bezier. Judge every widget tree the way a fluent platform user would: Material 3 on Android, iOS conventions on iOS, and no bounce anywhere.

## What the detector scans

`impeccable detect` walks `.dart` files with the other UI sources and runs the same rule ids in Dart syntax (`crates/detect/src/dart.rs`):

- **`bounce-easing`**: `Curves.bounceIn/bounceOut/bounceInOut`, `Curves.elasticIn/elasticOut/elasticInOut`, and the overshoot `Curves.easeOutBack`. Use exponential deceleration instead: `Curves.easeOutCubic`, `Curves.easeOutQuart`, `Curves.fastOutSlowIn`.
- **`overused-font`**: `fontFamily: 'Inter'` and friends, and `GoogleFonts.inter()` (Inter, Geist, Space Grotesk, Plus Jakarta Sans, …). Roboto is excluded on purpose: it is the Android system face, not a drift tell. Theme a brand face through the `TextTheme` roles rather than per-screen `fontFamily`.
- **`ai-color-palette`**: `LinearGradient(colors: [Colors.deepPurple, Colors.blueAccent])`-style purple gradient lists, and a purple literal on heading-size text (`headlineLarge`, the `display` roles, `titleLarge`, or `fontSize` ≥ 22 in the surrounding `TextStyle`).
- **`cream-palette`**: `scaffoldBackgroundColor: Color(0xFFFDF6EC)` and the like: the warm off-white default surface. Derive the scheme, don't default it.
- **`side-tab`**: a one-sided chromatic `Border`/`BorderDirectional` stripe (`left`, `right`, `start`, `end`) on a card **rounded away from it**, at 2px or more. It is the Dart twin of `border-left: 4px solid` on a rounded card; a square card's side rule is a classic accent and stays quiet. Accent cleanly instead: spread it along a Divider, an ink fill, or the container's own material tint, not a hard stripe down one edge of a rounded surface.

Anything the rules above cannot see stays with the LLM checks: nested `Card`/`Container` stacks, touch-target sizes, contrast pairs, and the type scale. The text engine reads lines, not geometry. `// impeccable-disable <rule>: <reason>` waives findings inline, exactly as in web files.

## Layout & structure

- **Material navigation, matched to size (Material apps).** `NavigationBar` on compact width; `NavigationRail` or `Drawer` on expanded width. The `android-layout-adaptive-nav` rule names the same duty for Android, and `MediaQuery` gets most of the way by default. A Cupertino app keeps the iOS navigation [ios.md](ios.md) specifies instead; never swap it for a Material bar.
- **One screen one `Scaffold`.** Nested `Scaffold`s fight the inset and snack-bar plumbing; reach for `NestedScrollView` or a plain `Builder` instead.
- **48×48 dp minimum** for every touch target. `IconButton` defaults there; custom `GestureDetector`s rarely do. A hit box smaller than the visual is the usual Flutter give-away.
- **Insets over fixed paddings.** `SafeArea` and `MediaQuery.paddingOf` beat hand-coded `EdgeInsets`, and `MediaQuery.viewInsetsOf` for the keyboard beats an estimated one.

## Color & theming

- **`ColorScheme.fromSeed` in a Material app, one source of truth.** Component colors read from `Theme.of(context).colorScheme` roles (`primary`, `surfaceContainerHighest`, `onSurfaceVariant`, `error`, …), never a raw `Color(0x…)` per widget. Raw hex is exactly what the `android-color-role-tokens` rule calls the last mile. A Cupertino app themes through `CupertinoTheme` and the semantic colors [ios.md](ios.md) names.
- **Dark mode is a scheme, not an `if`.** A quick `Color(0xFF121212)` inversion reads as one. Test both schemes from the widget tree, not the paint calls.

## Motion

- **Standard easing, `Duration`s in the 150–400 ms band** for `AnimatedContainer`, `AnimatedSwitcher`, and hero transitions. Bounce and elastic curves are a finding, not a style; the overshoot of `easeOutBack` reads the same way.
- **Implicit animation first.** An `Animated` twin beats an `AnimationController` scaffold unless the curve chain genuinely needs it. Fewer places for a stray `Curves.elasticOut` to hide.

## Verifying the build

- **Screenshots come from the device or emulator, never a browser.** `flutter test --update-goldens` pins widget goldens for CI. For the review pass, capture the real hardware states the platform reference names (see the verify section in [android.md](android.md)): dark mode, `textScale: 1.3`, and a small screen, where a fixed-width `SizedBox` tree shows itself.
- **The detector runs where Dart lives.** Point it at `lib/`. Findings from generated `*.g.dart` files belong to upstream packages; a `**/*.g.dart` entry under `detector.ignoreFiles` keeps them out of the scan instead of waiving each one.
