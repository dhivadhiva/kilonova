import 'package:flutter/material.dart';

import '../../l10n/generated/app_localizations.dart';
import '../../src/rust/api/sync.dart';
import '../../theme/theme.dart';
import '../../widgets/amount.dart';
import '../../widgets/kn_card.dart';
import '../../widgets/kn_icons.dart';

/// Transactions, newest first, as a lazy sliver card of rows: only the rows
/// near the viewport are ever built, so a history of thousands of entries
/// costs no more than a short one. Empty state is one line, no card. Place
/// directly among the `slivers` of a [CustomScrollView].
class HistoryList extends StatelessWidget {
  const HistoryList({super.key, required this.items, this.onOpen});

  final List<HistoryItem> items;

  /// Opens a transaction's details.
  final ValueChanged<HistoryItem>? onOpen;

  @override
  Widget build(BuildContext context) {
    final l = AppLocalizations.of(context);
    if (items.isEmpty) {
      return SliverToBoxAdapter(
        child: Text(
          l.historyEmpty,
          style: Theme.of(context).textTheme.bodyMedium,
        ),
      );
    }
    return SliverKnCard(
      itemCount: items.length,
      itemBuilder: (context, i) {
        final item = items[i];
        return _HistoryRow(
          item: item,
          onTap: onOpen == null ? null : () => onOpen!(item),
        );
      },
    );
  }
}

class _HistoryRow extends StatelessWidget {
  const _HistoryRow({required this.item, this.onTap});

  final HistoryItem item;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final l = AppLocalizations.of(context);
    final c = context.kn;
    final color = item.incoming ? c.received : c.text;
    final leading = item.pending
        ? Icon(Icons.schedule, size: 20, color: c.textSecondary)
        : KnIcon(item.incoming ? KnIcons.receive : KnIcons.send, color: color);
    final rightSide = item.pending
        ? l.historyPending
        : l.historyBlock(item.height.toString());
    final sentTo = item.sentTo;
    final base =
        item.note ??
        (item.miner
            ? l.historyMined
            : item.incoming
            ? l.historyReceived
            : sentTo == null
            ? l.historySent
            : l.historyTo(_short(sentTo)));
    final secondLine = item.locked ? l.historyDetailLocked(base) : base;

    return KnRow(
      leading: leading,
      title: Row(
        children: [
          Expanded(
            child: AmountText(
              item.amount,
              prefix: item.incoming ? '+' : '-',
              color: color,
              style: Theme.of(context).textTheme.bodyLarge,
            ),
          ),
          Text(
            rightSide,
            style: monoStyle(context, size: 13, color: c.textSecondary),
          ),
        ],
      ),
      subtitle: Text(secondLine, maxLines: 1, overflow: TextOverflow.ellipsis),
      onTap: onTap,
    );
  }

  /// A long address shortened to its ends; contact names stay whole.
  static String _short(String s) =>
      s.length > 24 ? '${s.substring(0, 8)}…${s.substring(s.length - 8)}' : s;
}
