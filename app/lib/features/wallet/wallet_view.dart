import 'package:flutter/material.dart';

import '../../l10n/generated/app_localizations.dart';
import '../../src/rust/api/requests.dart';
import '../../src/rust/api/sync.dart';
import '../../src/rust/api/wallets.dart';
import '../../theme/theme.dart';
import '../../theme/tokens.dart';
import '../../widgets/amount.dart';
import '../../widgets/kn_button.dart';
import '../../widgets/kn_card.dart';
import '../../widgets/kn_icons.dart';
import '../../widgets/mode_label.dart';
import '../cold/cold_screens.dart';
import '../send/send_screen.dart';
import '../wallets/wallet_registry.dart';
import 'history_list.dart';
import 'receive_screen.dart';
import 'request_screen.dart';
import 'sync_panel.dart';
import 'tx_details_screen.dart';
import 'wallet_dialogs.dart';

/// An unlocked wallet: name and header, balance, sync status, actions and
/// history. Receiving addresses live in [ReceiveScreen].
class WalletView extends StatefulWidget {
  const WalletView({
    super.key,
    required this.wallet,
    required this.registry,
    this.showHeader = true,
  });

  final OpenWallet wallet;
  final WalletRegistry registry;

  /// False when the caller already shows the name and the more menu in its
  /// own app bar (the phone wallet page), so this does not repeat them.
  final bool showHeader;

  @override
  State<WalletView> createState() => _WalletViewState();
}

class _WalletViewState extends State<WalletView> {
  // The wallet page used to call summary(), requests(), history() and
  // coinsWithoutKeyImages() straight from build(), every time any sync
  // event arrived (progress pings included). On a wallet with a long
  // history that is real, repeated FFI work for data that has not changed.
  // Instead, the sync listenable is watched by hand: every event still
  // redraws the sync line (cheap), but the FFI-backed fields below are
  // only refetched when the scanned height actually moves, or after an
  // action here that is known to have changed them.
  late WalletSummary _summary = widget.wallet.summary();
  SyncEvent? _event;
  List<HistoryItem> _history = const [];
  List<RequestRow> _requests = const [];
  int _needsKeyImages = 0;
  BigInt? _dataHeight;

  @override
  void initState() {
    super.initState();
    _loadData();
    if (!_summary.cold) {
      _event = widget.registry.syncOf(_summary.id).value;
      widget.registry.syncOf(_summary.id).addListener(_onSyncEvent);
    }
  }

  @override
  void dispose() {
    if (!_summary.cold) {
      widget.registry.syncOf(_summary.id).removeListener(_onSyncEvent);
    }
    super.dispose();
  }

  void _loadData() {
    _summary = widget.wallet.summary();
    if (_summary.cold) return;
    _history = widget.wallet.history();
    _requests = widget.wallet.requests();
    _needsKeyImages = _summary.viewOnly
        ? widget.wallet.coinsWithoutKeyImages()
        : 0;
  }

  void _onSyncEvent() {
    final event = widget.registry.syncOf(_summary.id).value;
    final height = event?.scanned;
    final changed = height != _dataHeight;
    setState(() {
      _event = event;
      if (changed) {
        _dataHeight = height;
        _loadData();
      }
    });
  }

  Future<void> _send() async {
    final l = AppLocalizations.of(context);
    final sent = await Navigator.of(context).push<bool>(
      MaterialPageRoute(
        builder: (_) =>
            SendScreen(wallet: widget.wallet, registry: widget.registry),
      ),
    );
    if (sent != true || !mounted) return;
    setState(_loadData);
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(l.sentNotice)));
  }

  Future<void> _receive() => openReceiveScreen(context, wallet: widget.wallet);

  Future<void> _scanColdRequest() =>
      coldScanRequest(context, widget.wallet, widget.registry);

  Future<void> _pair() => coldPair(context, widget.wallet, widget.registry);

  Future<void> _syncWithOffline() async {
    await coldSyncWithOffline(context, widget.wallet, widget.registry);
    if (mounted) setState(_loadData);
  }

  Widget _header(BuildContext context, WalletSummary summary) {
    final l = AppLocalizations.of(context);
    final text = Theme.of(context).textTheme;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (widget.showHeader) ...[
          Row(
            children: [
              Expanded(child: Text(summary.name, style: text.headlineSmall)),
              WalletMenu(wallet: widget.wallet, registry: widget.registry),
            ],
          ),
          const SizedBox(height: KnSpace.xs),
        ],
        Text(
          [
            // A cold wallet never syncs, so its mode would mislead.
            summary.cold ? l.coldOfflineLabel : summary.mode.label(context),
            if (summary.viewOnly) l.viewOnlyTag,
          ].join(' · '),
          style: text.bodySmall,
        ),
      ],
    );
  }

  Widget _coldBody(BuildContext context, WalletSummary summary) {
    final l = AppLocalizations.of(context);
    final text = Theme.of(context).textTheme;
    return ListView(
      padding: EdgeInsets.zero,
      children: [
        _header(context, summary),
        const SizedBox(height: KnSpace.xl),
        KnCard(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  const KnIcon(KnIcons.lock),
                  const SizedBox(width: KnSpace.sm),
                  Text(l.coldOfflineTitle, style: text.titleMedium),
                ],
              ),
              const SizedBox(height: KnSpace.sm),
              Text(l.coldOfflineBody, style: text.bodySmall),
            ],
          ),
        ),
        const SizedBox(height: KnSpace.lg),
        KnButton.primary(
          l.coldScanRequestAction,
          icon: const KnIcon(KnIcons.scan),
          onPressed: _scanColdRequest,
          expand: true,
        ),
        const SizedBox(height: KnSpace.sm),
        KnButton.secondary(l.coldPairAction, onPressed: _pair, expand: true),
        const SizedBox(height: KnSpace.sm),
        KnButton.secondary(
          l.receiveTitle,
          icon: const KnIcon(KnIcons.receive),
          onPressed: _receive,
          expand: true,
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final l = AppLocalizations.of(context);
    final text = Theme.of(context).textTheme;
    final summary = _summary;

    if (summary.cold) return _coldBody(context, summary);

    final requests = _requests;

    return CustomScrollView(
      slivers: [
        SliverToBoxAdapter(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              _header(context, summary),
              const SizedBox(height: KnSpace.xl),
              BalanceBlock(
                wallet: widget.wallet,
                prices: summary.network.isTestNetwork()
                    ? null
                    : widget.registry.price,
              ),
              const SizedBox(height: KnSpace.lg),
              if (_needsKeyImages > 0) ...[
                KnCard(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        l.coldSyncNeededCard(_needsKeyImages),
                        style: text.bodyMedium,
                      ),
                      const SizedBox(height: KnSpace.sm),
                      KnButton.secondary(
                        l.coldSyncWithOfflineAction,
                        onPressed: _syncWithOffline,
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: KnSpace.lg),
              ],
              Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  KnButton.primary(
                    l.sendAction,
                    icon: const KnIcon(KnIcons.send),
                    onPressed: _send,
                  ),
                  const SizedBox(width: KnSpace.sm),
                  KnButton.secondary(
                    l.receiveTitle,
                    icon: const KnIcon(KnIcons.receive),
                    onPressed: _receive,
                  ),
                ],
              ),
              const SizedBox(height: KnSpace.lg),
              RepaintBoundary(
                child: SyncLine(
                  wallet: widget.wallet,
                  event: _event,
                  onRetry: () => widget.registry.startSync(summary.id),
                  registry: widget.registry,
                ),
              ),
              if (requests.isNotEmpty) ...[
                const SizedBox(height: KnSpace.xl),
                Eyebrow(l.requestsTitle),
                const SizedBox(height: KnSpace.sm),
              ],
            ],
          ),
        ),
        if (requests.isNotEmpty)
          SliverKnCard(
            itemCount: requests.length,
            itemBuilder: (context, i) {
              final r = requests[i];
              return KnRow(
                title: Text(r.label.isEmpty ? l.requestDefaultLabel : r.label),
                subtitle: Text(
                  requestStatusText(context, r),
                  style: r.status == RequestStatus.paid
                      ? TextStyle(color: context.kn.received)
                      : null,
                ),
                trailing: AmountText(r.amount),
                onTap: () async {
                  await Navigator.of(context).push(
                    MaterialPageRoute<void>(
                      builder: (_) =>
                          RequestScreen(wallet: widget.wallet, request: r),
                    ),
                  );
                  if (mounted) setState(_loadData);
                },
              );
            },
          ),
        SliverToBoxAdapter(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const SizedBox(height: KnSpace.xl),
              Eyebrow(l.historyTitle),
              const SizedBox(height: KnSpace.sm),
            ],
          ),
        ),
        HistoryList(
          items: _history,
          onOpen: (item) async {
            await Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => TxDetailsScreen(
                  wallet: widget.wallet,
                  item: item,
                  biometric: widget.registry.biometric,
                ),
              ),
            );
            if (mounted) setState(_loadData);
          },
        ),
      ],
    );
  }
}
