import 'package:flutter/material.dart';

// Impeccable Dart flag fixture: every slop tell this screen ships.

class SlopLanding extends StatelessWidget {
  const SlopLanding({super.key});

  @override
  Widget build(BuildContext context) {
    final headline = GoogleFonts.spaceGroteskTextTheme();
    return Scaffold(
      backgroundColor: const Color(0xFFFDF6EC),
      appBar: AppBar(
        title: DefaultTextStyle(
          style: headline.headlineLarge!.copyWith(
            color: const Color(0xFF9C27B0),
          ),
          child: const Text('Ship faster with Impeccable'),
        ),
      ),
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const SizedBox(
            child: Text(
              'AI-generated marketing copy',
              style: TextStyle(fontFamily: 'Inter', fontSize: 14),
            ),
          ),
          GestureDetector(
            onTap: () {},
            child: AnimatedContainer(
              duration: const Duration(milliseconds: 320),
              curve: Curves.bounceOut,
              decoration: const BoxDecoration(
                borderRadius: BorderRadius.circular(12),
                border: Border(
                  left: BorderSide(color: Color(0xFF8B5CF6), width: 4),
                ),
              ),
              child: const Icon(Icons.rocket_launch_rounded),
            ),
          ),
          const SizedBox(
            child: DecoratedBox(
              decoration: BoxDecoration(
                gradient: LinearGradient(
                  colors: [Colors.deepPurple, Colors.blueAccent],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
