import 'package:flutter/material.dart';

// Impeccable Dart pass fixture: trigger words live in comments and strings,
// and every real property below is a deliberate non-tell.
// Commented tells that must stay silent: Curves.bounceOut, fontFamily: 'Inter',
// Colors.deepPurple gradient, scaffoldBackgroundColor: Color(0xFFFDF6EC).

final bodyFace = GoogleFonts.robotoTextTheme();

class CleanDashboard extends StatelessWidget {
  const CleanDashboard({super.key});

  String slug(String url) => 'see $url'; // hrefs like https://example.com/docs

  @override
  Widget build(BuildContext context) {
    const scheme = ColorScheme.fromSeed(seedColor: Color(0xFF1B4B5A));
    return Theme(
      data: ThemeData(colorScheme: scheme, useMaterial3: true),
      child: Scaffold(
        appBar: AppBar(
          title: const Text(
            'Reports',
            style: TextStyle(
              fontFamilyFallback: ['Inter'],
              fontSize: 22,
              fontWeight: FontWeight.w600,
            ),
          ),
          actions: [
            IconButton(
              icon: const Icon(Icons.chevron_right),
              constraints: const BoxConstraints(minWidth: 48, minHeight: 48),
              onPressed: () {},
            ),
          ],
        ),
        body: Column(
          children: [
            Container(
              padding: const EdgeInsets.all(16),
              decoration: BoxDecoration(
                color: const Color(0xFFF8F6F4),
                border: Border.all(color: const Color(0xFF1B4B5A), width: 1),
                borderRadius: BorderRadius.circular(8),
              ),
              child: const Text('Overview', style: TextStyle(fontSize: 16)),
            ),
            AnimatedSwitcher(
              duration: const Duration(milliseconds: 250),
              child: Container(
                decoration: const BoxDecoration(
                  gradient: LinearGradient(
                    colors: [Colors.teal, Colors.black87],
                  ),
                ),
                child: const Text(
                  'Usage',
                  style: TextStyle(color: Colors.black87),
                ),
              ),
            ),
            ElevatedButton(
              style: ElevatedButton.styleFrom(fixedSize: const Size(240, 48)),
              onPressed: () {},
              child: const Text('Open'),
            ),
            Divider(
              thickness: 0.5,
              color: scheme.outline,
            ),
          ],
        ),
      ),
    );
  }
}
