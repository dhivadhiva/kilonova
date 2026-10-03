import 'package:flutter/material.dart';

import '../../l10n/generated/app_localizations.dart';
import '../../src/rust/api/coins.dart';
import '../../src/rust/api/wallets.dart';
import '../../theme/theme.dart';
import '../../theme/tokens.dart';
import '../../widgets/amount.dart';
import '../../widgets/kn_button.dart';
import '../../widgets/kn_card.dart';
import '../send/send_screen.dart';
import '../wallets/wallet_registry.dart';

/// The wallet's unspent outputs: freeze one, or pick several to spend
/// together.
class CoinsScreen extends StatefulWidget {
  const CoinsScreen({super.key, required this.wallet, required this.registry});

  final OpenWallet wallet;
  final WalletRegistry registry;

  @override
  State<CoinsScreen> createState() => _CoinsScreenState();
}

class _CoinsScreenState extends State<CoinsScreen> {
  late List<CoinRow> _coins = widget.wallet.coins();
  final Set<String> _selected = {};

  Future<void> _toggleFrozen(CoinRow coin) async {
    await widget.wallet.setCoinFrozen(key: coin.key, frozen: !coin.frozen);
    setState(() {
      _coins = widget.wallet.coins();
      if (!coin.frozen) _selected.remove(coin.key);
    });
  }

  void _toggleSelected(CoinRow coin) {
    if (coin.frozen || coin.locked) return;
    setState(() {
      if (!_selected.remove(coin.key)) _selected.add(coin.key);
    });
  }

  Future<void> _sendFromSelected() async {
    final sent = await Navigator.of(context).push<bool>(
      MaterialPageRoute(
        builder: (_) => SendScreen(
          wallet: widget.wallet,
          registry: widget.registry,
          coins: _selected.toList(),
        ),
      ),
    );
    if (sent == true && mounted) {
      setState(() {
        _coins = widget.wallet.coins();
        _selected.clear();
      });
    }
  }

  String _subtitle(BuildContext context, CoinRow coin) {
    final l = AppLocalizations.of(context);
    final address = coin.subaddressIndex == 0
        ? l.primaryAddressLabel
        : l.subaddressLabel(coin.subaddressIndex);
    var line = [address, l.historyBlock(coin.height.toString())].join(' · ');
    if (coin.locked) line += ', ${l.coinsLockedTag}';
    if (coin.frozen) line += ', ${l.coinsFrozenTag}';
    return line;
  }

  @override
  Widget build(BuildContext context) {
    final l = AppLocalizations.of(context);
    final text = Theme.of(context).textTheme;
    final c = context.kn;
    final selectedTotal = _coins
        .where((coin) => _selected.contains(coin.key))
        .fold(BigInt.zero, (sum, coin) => sum + coin.amount);

    return Scaffold(
      appBar: AppBar(title: Text(l.coinsTitle)),
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Expanded(
            child: Align(
              alignment: Alignment.topLeft,
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: 760),
                child: _coins.isEmpty
                    ? Padding(
                        padding: const EdgeInsets.all(KnSpace.lg),
                        child: Text(l.coinsEmpty, style: text.bodyMedium),
                      )
                    // A long-lived wallet can accumulate thousands of
                    // coins; CustomScrollView + SliverKnCard builds only
                    // the rows near the viewport, instead of every row up
                    // front.
                    : CustomScrollView(
                        slivers: [
                          SliverPadding(
                            padding: const EdgeInsets.all(KnSpace.lg),
                            sliver: SliverKnCard(
                              itemCount: _coins.length,
                              itemBuilder: (context, i) {
                                final coin = _coins[i];
                                return KnRow(
                                  leading: Checkbox(
                                    value: _selected.contains(coin.key),
                                    onChanged: coin.frozen || coin.locked
                                        ? null
                                        : (_) => _toggleSelected(coin),
                                  ),
                                  title: AmountText(coin.amount),
                                  subtitle: Text(_subtitle(context, coin)),
                                  trailing: KnIconButton(
                                    icon: const Icon(Icons.ac_unit),
                                    tooltip: coin.frozen
                                        ? l.coinsUnfreezeTooltip
                                        : l.coinsFreezeTooltip,
                                    onPressed: () => _toggleFrozen(coin),
                                  ),
                                  onTap: coin.frozen || coin.locked
                                      ? null
                                      : () => _toggleSelected(coin),
                                );
                              },
                            ),
                          ),
                        ],
                      ),
              ),
            ),
          ),
          if (_selected.isNotEmpty)
            Container(
              padding: const EdgeInsets.all(KnSpace.md),
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: c.border)),
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      l.coinsSelectedSummary(
                        _selected.length,
                        formatXmr(selectedTotal),
                      ),
                      style: text.bodyMedium,
                    ),
                  ),
                  const SizedBox(width: KnSpace.md),
                  KnButton.primary(
                    l.coinsSendFromAction,
                    onPressed: _sendFromSelected,
                  ),
                ],
              ),
            ),
        ],
      ),
    );
  }
}
