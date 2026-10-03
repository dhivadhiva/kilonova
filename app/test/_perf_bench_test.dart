// Temporary benchmark, not part of the suite. Measures how many history
// rows actually get built into the widget tree for a long history, and how
// long the first pump takes. Run with:
//   flutter test test/_perf_bench_test.dart
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:kilonova/features/wallet/history_list.dart';
import 'package:kilonova/l10n/generated/app_localizations.dart';
import 'package:kilonova/src/rust/api/sync.dart';
import 'package:kilonova/theme/theme.dart';
import 'package:kilonova/widgets/kn_card.dart' show KnRow;

int _rssKb() {
  final status = File('/proc/self/status').readAsLinesSync();
  for (final l in status) {
    if (l.startsWith('VmRSS:')) {
      return int.parse(RegExp(r'\d+').firstMatch(l)!.group(0)!);
    }
  }
  return -1;
}

HistoryItem _item(int i) => HistoryItem(
  txHash: 'tx$i' * 4,
  height: BigInt.from(1000 + i),
  incoming: i.isEven,
  amount: BigInt.from(1000000000 + i),
  miner: false,
  locked: false,
  subaddressIndex: 0,
  pending: false,
  note: null,
  sentTo: i.isEven ? null : 'addr$i',
);

void main() {
  testWidgets('history list build cost with 2000 items', (tester) async {
    final items = List.generate(2000, _item);
    final rssBefore = _rssKb();
    final sw = Stopwatch()..start();
    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(Brightness.light),
        localizationsDelegates: AppLocalizations.localizationsDelegates,
        supportedLocales: AppLocalizations.supportedLocales,
        home: Scaffold(
          body: SizedBox(
            height: 600,
            child: SingleChildScrollView(child: HistoryList(items: items)),
          ),
        ),
      ),
    );
    sw.stop();
    final rssAfter = _rssKb();
    final builtRows = find.byType(KnRow).evaluate().length;
    // ignore: avoid_print
    print(
      'PERF history(2000): firstPumpMs=${sw.elapsedMilliseconds} '
      'builtRows=$builtRows rssDeltaKb=${rssAfter - rssBefore} '
      'rssAfterKb=$rssAfter',
    );
  });
}
