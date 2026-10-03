import 'package:flutter/material.dart';

import '../theme/theme.dart';
import '../theme/tokens.dart';

/// The only card: an opaque `surface` block with a hairline outline and no
/// shadow. Rows inside it are separated with [KnDivider], see [withDividers].
class KnCard extends StatelessWidget {
  const KnCard({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(20),
  });

  final Widget child;

  /// Use [EdgeInsets.zero] for a card of [KnRow]s.
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final c = context.kn;
    return Container(
      clipBehavior: Clip.antiAlias,
      decoration: BoxDecoration(
        color: c.surface,
        border: Border.all(color: c.border),
        borderRadius: BorderRadius.circular(KnRadius.md),
      ),
      // Rows inside draw their hover fill on this, above the card color.
      child: Material(
        type: MaterialType.transparency,
        child: Padding(padding: padding, child: child),
      ),
    );
  }
}

/// The lazy version of [KnCard] for a long list of rows: the same opaque
/// `surface` block, hairline outline and dividers, but as a sliver whose
/// rows are only built near the viewport. Place directly among the
/// `slivers` of a [CustomScrollView]; does not take its own padding (wrap
/// in [SliverPadding] if needed).
class SliverKnCard extends StatelessWidget {
  const SliverKnCard({super.key, required this.itemCount, required this.itemBuilder});

  final int itemCount;

  /// Builds the row at [index]; dividers between rows are added for you.
  final Widget Function(BuildContext context, int index) itemBuilder;

  @override
  Widget build(BuildContext context) {
    final c = context.kn;
    return DecoratedSliver(
      decoration: BoxDecoration(
        color: c.surface,
        border: Border.all(color: c.border),
        borderRadius: BorderRadius.circular(KnRadius.md),
      ),
      sliver: SliverList(
        delegate: SliverChildBuilderDelegate(
          (context, i) => Material(
            type: MaterialType.transparency,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                if (i > 0) const KnDivider(),
                itemBuilder(context, i),
              ],
            ),
          ),
          childCount: itemCount,
        ),
      ),
    );
  }
}

/// A 1px `border` hairline.
class KnDivider extends StatelessWidget {
  const KnDivider({super.key, this.indent = 0});

  final double indent;

  @override
  Widget build(BuildContext context) {
    return Divider(
      height: 1,
      thickness: 1,
      indent: indent,
      color: context.kn.border,
    );
  }
}

/// [children] with a [KnDivider] between each pair.
List<Widget> withDividers(Iterable<Widget> children) => [
  for (final (i, child) in children.indexed) ...[
    if (i > 0) const KnDivider(),
    child,
  ],
];

/// The upper-case label above a block, such as "BALANCE". The only
/// upper-case text in the app; never a badge.
class Eyebrow extends StatelessWidget {
  const Eyebrow(this.text, {super.key});

  final String text;

  @override
  Widget build(BuildContext context) {
    return Text(
      text.toUpperCase(),
      style: Theme.of(
        context,
      ).textTheme.labelMedium!.copyWith(letterSpacing: 0.6),
    );
  }
}

/// A list row, 52px at least. [selected] fills it with `surfaceRaised` and
/// marks the left edge with a 3px gold bar; hovering fills it too.
class KnRow extends StatelessWidget {
  const KnRow({
    super.key,
    required this.title,
    this.subtitle,
    this.leading,
    this.trailing,
    this.selected = false,
    this.onTap,
    this.padding = const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
  });

  /// Usually a [Text]; styled `bodyLarge` unless it sets its own style.
  final Widget title;

  /// Usually a [Text]; styled `bodySmall`.
  final Widget? subtitle;
  final Widget? leading;
  final Widget? trailing;
  final bool selected;
  final VoidCallback? onTap;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final c = context.kn;
    final text = Theme.of(context).textTheme;
    final leading = this.leading;
    final subtitle = this.subtitle;
    final trailing = this.trailing;

    final content = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 52),
      child: Padding(
        padding: padding,
        child: Row(
          children: [
            if (leading != null) ...[
              IconTheme.merge(
                data: IconThemeData(color: c.textSecondary, size: 20),
                child: leading,
              ),
              const SizedBox(width: 12),
            ],
            Expanded(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  DefaultTextStyle.merge(
                    style: text.bodyLarge,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    child: title,
                  ),
                  if (subtitle != null)
                    DefaultTextStyle.merge(
                      style: text.bodySmall,
                      child: subtitle,
                    ),
                ],
              ),
            ),
            if (trailing != null) ...[
              const SizedBox(width: 12),
              IconTheme.merge(
                data: IconThemeData(color: c.textSecondary, size: 20),
                child: trailing,
              ),
            ],
          ],
        ),
      ),
    );

    return Semantics(
      selected: selected,
      child: Material(
        color: selected ? c.surfaceRaised : null,
        type: selected ? MaterialType.canvas : MaterialType.transparency,
        child: InkWell(
          onTap: onTap,
          child: Stack(
            children: [
              content,
              if (selected)
                Positioned(
                  left: 0,
                  top: 0,
                  bottom: 0,
                  width: 3,
                  child: ColoredBox(color: c.accent),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A label and value on one 36px line, for review and detail screens. Put
/// several in a column with [withDividers].
class KeyValue extends StatelessWidget {
  const KeyValue({
    super.key,
    required this.label,
    required this.value,
    this.strong = false,
  });

  final String label;

  /// Usually a [Text] or an AmountText; set in the mono face.
  final Widget value;

  /// Weight 500 on both sides, for the line that matters most.
  final bool strong;

  @override
  Widget build(BuildContext context) {
    final c = context.kn;
    final weight = strong ? FontWeight.w500 : FontWeight.w400;
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: 36),
      child: Row(
        children: [
          Expanded(
            child: Text(
              label,
              style: Theme.of(context).textTheme.bodyMedium!.copyWith(
                color: c.textSecondary,
                fontWeight: weight,
              ),
            ),
          ),
          const SizedBox(width: 16),
          DefaultTextStyle.merge(
            style: monoStyle(context).copyWith(fontWeight: weight),
            child: value,
          ),
        ],
      ),
    );
  }
}
