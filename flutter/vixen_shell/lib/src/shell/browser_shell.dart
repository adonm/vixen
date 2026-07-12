import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bridge/browser_models.dart';
import 'shell_coordinator.dart';
import 'texture_presenter.dart';

export 'texture_presenter.dart' show BrowserContentSurface;

final class BrowserShell extends StatefulWidget {
  const BrowserShell({required this.coordinator, super.key});

  final ShellCoordinator coordinator;

  @override
  State<BrowserShell> createState() => _BrowserShellState();
}

final class _BrowserShellState extends State<BrowserShell> {
  final TextEditingController _addressController = TextEditingController();
  final FocusNode _addressFocus = FocusNode();

  @override
  void initState() {
    super.initState();
    widget.coordinator.addListener(_coordinatorChanged);
    _coordinatorChanged();
    unawaited(widget.coordinator.start());
  }

  @override
  void didUpdateWidget(BrowserShell oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.coordinator == widget.coordinator) return;
    oldWidget.coordinator.removeListener(_coordinatorChanged);
    widget.coordinator.addListener(_coordinatorChanged);
    _coordinatorChanged();
    unawaited(widget.coordinator.start());
  }

  void _coordinatorChanged() {
    if (!_addressFocus.hasFocus) {
      final url = widget.coordinator.selectedContext?.url ?? '';
      _addressController.value = TextEditingValue(
        text: url,
        selection: TextSelection.collapsed(offset: url.length),
      );
    }
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    widget.coordinator.removeListener(_coordinatorChanged);
    _addressController.dispose();
    _addressFocus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final coordinator = widget.coordinator;
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.keyL, control: true):
            _focusAddress,
        const SingleActivator(LogicalKeyboardKey.keyT, control: true): () {
          unawaited(coordinator.newTab());
        },
        const SingleActivator(LogicalKeyboardKey.keyW, control: true): () {
          final id = coordinator.activeContextId;
          if (id != null) unawaited(coordinator.closeTab(id));
        },
        const SingleActivator(LogicalKeyboardKey.keyR, control: true): () {
          unawaited(coordinator.reload());
        },
        const SingleActivator(LogicalKeyboardKey.escape): _escape,
        const SingleActivator(LogicalKeyboardKey.arrowLeft, alt: true): () {
          unawaited(coordinator.goBack());
        },
        const SingleActivator(LogicalKeyboardKey.arrowRight, alt: true): () {
          unawaited(coordinator.goForward());
        },
      },
      child: Focus(
        autofocus: true,
        child: Scaffold(
          body: SafeArea(
            child: Column(
              children: [
                if (coordinator.errorMessage case final message?)
                  _ErrorBanner(
                    message: message,
                    onDismiss: coordinator.clearError,
                  ),
                _TabStrip(coordinator: coordinator),
                _Toolbar(
                  coordinator: coordinator,
                  addressController: _addressController,
                  addressFocus: _addressFocus,
                  onSubmitted: (value) {
                    unawaited(coordinator.navigate(value));
                    _addressFocus.unfocus();
                  },
                  onShowAbout: () => _showAbout(context),
                  onShowShortcuts: () => _showShortcuts(context),
                ),
                _SelectedProgress(context: coordinator.selectedContext),
                Expanded(
                  child: BrowserContentSurface(
                    contextState: coordinator.selectedContext,
                    frame: coordinator.frame,
                    accessibility: coordinator.accessibility,
                    onPhysicalViewportChanged:
                        coordinator.updatePhysicalViewport,
                    onMouseEvent: (eventType, event) {
                      unawaited(
                        coordinator.dispatchMouseEvent(eventType, event),
                      );
                    },
                    onKeyEvent: (eventType, event) {
                      unawaited(coordinator.dispatchKeyEvent(eventType, event));
                    },
                    onSemanticTap: (snapshot, node) {
                      unawaited(
                        coordinator.dispatchSemanticTap(snapshot, node),
                      );
                    },
                    onSemanticFocus: (snapshot, node) {
                      unawaited(
                        coordinator.dispatchSemanticFocus(snapshot, node),
                      );
                    },
                    onSemanticSetValue: (snapshot, node, value) {
                      unawaited(
                        coordinator.dispatchSemanticSetValue(
                          snapshot,
                          node,
                          value,
                        ),
                      );
                    },
                    onSemanticAdjustment: (snapshot, node, increase) {
                      unawaited(
                        coordinator.dispatchSemanticAdjustment(
                          snapshot,
                          node,
                          increase: increase,
                        ),
                      );
                    },
                  ),
                ),
                _StatusBar(status: coordinator.selectedStatus),
              ],
            ),
          ),
        ),
      ),
    );
  }

  void _focusAddress() {
    _addressFocus.requestFocus();
    _addressController.selection = TextSelection(
      baseOffset: 0,
      extentOffset: _addressController.text.length,
    );
  }

  void _escape() {
    if (_addressFocus.hasFocus) {
      final url = widget.coordinator.selectedContext?.url ?? '';
      _addressController.text = url;
      _addressFocus.unfocus();
    } else {
      unawaited(widget.coordinator.stop());
    }
  }
}

final class _TabStrip extends StatelessWidget {
  const _TabStrip({required this.coordinator});

  final ShellCoordinator coordinator;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.surfaceContainer,
      child: SizedBox(
        height: 44,
        child: Row(
          children: [
            Expanded(
              child: ListView.builder(
                scrollDirection: Axis.horizontal,
                itemCount: coordinator.contexts.length,
                itemBuilder: (context, index) {
                  final tab = coordinator.contexts[index];
                  final selected = tab.contextId == coordinator.activeContextId;
                  return Semantics(
                    selected: selected,
                    button: true,
                    label: 'Tab ${tab.displayTitle}',
                    child: InkWell(
                      key: ValueKey('tab-${tab.contextId}'),
                      onTap: () =>
                          unawaited(coordinator.activateTab(tab.contextId)),
                      child: Container(
                        width: 210,
                        padding: const EdgeInsets.only(left: 14),
                        decoration: BoxDecoration(
                          color: selected
                              ? colors.surface
                              : colors.surfaceContainer,
                          border: Border(
                            bottom: BorderSide(
                              color: selected
                                  ? colors.primary
                                  : Colors.transparent,
                              width: 2,
                            ),
                          ),
                        ),
                        child: Row(
                          children: [
                            if (tab.isLoading)
                              const SizedBox.square(
                                dimension: 14,
                                child: CircularProgressIndicator(
                                  strokeWidth: 2,
                                ),
                              )
                            else
                              const Icon(Icons.public, size: 16),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                tab.displayTitle,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            IconButton(
                              key: ValueKey('close-tab-${tab.contextId}'),
                              tooltip: 'Close tab',
                              visualDensity: VisualDensity.compact,
                              icon: const Icon(Icons.close, size: 17),
                              onPressed: () => unawaited(
                                coordinator.closeTab(tab.contextId),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                  );
                },
              ),
            ),
            IconButton(
              key: const Key('new-tab'),
              tooltip: 'New tab',
              onPressed: () => unawaited(coordinator.newTab()),
              icon: const Icon(Icons.add),
            ),
            const SizedBox(width: 4),
          ],
        ),
      ),
    );
  }
}

final class _Toolbar extends StatelessWidget {
  const _Toolbar({
    required this.coordinator,
    required this.addressController,
    required this.addressFocus,
    required this.onSubmitted,
    required this.onShowAbout,
    required this.onShowShortcuts,
  });

  final ShellCoordinator coordinator;
  final TextEditingController addressController;
  final FocusNode addressFocus;
  final ValueChanged<String> onSubmitted;
  final VoidCallback onShowAbout;
  final VoidCallback onShowShortcuts;

  @override
  Widget build(BuildContext context) {
    final tab = coordinator.selectedContext;
    return Material(
      elevation: 1,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 7),
        child: Row(
          children: [
            IconButton(
              key: const Key('back'),
              tooltip: 'Back (Alt+Left)',
              onPressed: tab?.canGoBack == true
                  ? () => unawaited(coordinator.goBack())
                  : null,
              icon: const Icon(Icons.arrow_back),
            ),
            IconButton(
              key: const Key('forward'),
              tooltip: 'Forward (Alt+Right)',
              onPressed: tab?.canGoForward == true
                  ? () => unawaited(coordinator.goForward())
                  : null,
              icon: const Icon(Icons.arrow_forward),
            ),
            IconButton(
              key: const Key('reload-stop'),
              tooltip: tab?.isLoading == true
                  ? 'Stop (Escape)'
                  : 'Reload (Ctrl+R)',
              onPressed: tab == null
                  ? null
                  : () => unawaited(
                      tab.isLoading ? coordinator.stop() : coordinator.reload(),
                    ),
              icon: Icon(tab?.isLoading == true ? Icons.close : Icons.refresh),
            ),
            const SizedBox(width: 6),
            Expanded(
              child: TextField(
                key: const Key('address-field'),
                controller: addressController,
                focusNode: addressFocus,
                enabled: tab != null,
                onSubmitted: onSubmitted,
                textInputAction: TextInputAction.go,
                decoration: InputDecoration(
                  hintText: 'Search or enter address',
                  prefixIcon: const Icon(Icons.language, size: 19),
                  isDense: true,
                  filled: true,
                  fillColor: Theme.of(context).colorScheme.surfaceContainerLow,
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(20),
                    borderSide: BorderSide.none,
                  ),
                ),
              ),
            ),
            const SizedBox(width: 6),
            PopupMenuButton<_MenuAction>(
              key: const Key('main-menu'),
              tooltip: 'Vixen menu',
              onSelected: (action) {
                switch (action) {
                  case _MenuAction.shortcuts:
                    onShowShortcuts();
                  case _MenuAction.about:
                    onShowAbout();
                }
              },
              itemBuilder: (context) => const [
                PopupMenuItem(
                  value: _MenuAction.shortcuts,
                  child: Text('Keyboard shortcuts'),
                ),
                PopupMenuItem(
                  value: _MenuAction.about,
                  child: Text('About Vixen'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

enum _MenuAction { shortcuts, about }

final class _SelectedProgress extends StatelessWidget {
  const _SelectedProgress({required this.context});
  final BrowsingContextState? context;

  @override
  Widget build(BuildContext context) {
    final state = this.context;
    if (state?.isLoading != true) return const SizedBox(height: 2);
    final progress = state!.loadProgress;
    return LinearProgressIndicator(
      key: const Key('page-progress'),
      minHeight: 2,
      value: progress > 0 && progress <= 1 ? progress : null,
    );
  }
}

final class _StatusBar extends StatelessWidget {
  const _StatusBar({required this.status});
  final String status;

  @override
  Widget build(BuildContext context) {
    return Container(
      key: const Key('status-bar'),
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      color: Theme.of(context).colorScheme.surfaceContainerLow,
      child: Text(
        status,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: Theme.of(context).textTheme.labelSmall,
      ),
    );
  }
}

final class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.message, required this.onDismiss});
  final String message;
  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      key: const Key('error-banner'),
      color: colors.errorContainer,
      child: ListTile(
        dense: true,
        leading: Icon(Icons.error_outline, color: colors.onErrorContainer),
        title: Text(message),
        trailing: IconButton(
          tooltip: 'Dismiss error',
          onPressed: onDismiss,
          icon: const Icon(Icons.close),
        ),
      ),
    );
  }
}

void _showAbout(BuildContext context) {
  showAboutDialog(
    context: context,
    applicationName: 'Vixen',
    applicationVersion: '0.1.0',
    applicationLegalese:
        'Flutter presents browser chrome. BrowserCore owns browser state.',
  );
}

void _showShortcuts(BuildContext context) {
  showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: const Text('Keyboard shortcuts'),
      content: const Text(
        'Ctrl+L  Focus address\n'
        'Ctrl+T  New tab\n'
        'Ctrl+W  Close tab\n'
        'Ctrl+R  Reload\n'
        'Escape  Stop or leave address\n'
        'Alt+Left  Back\n'
        'Alt+Right  Forward',
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Close'),
        ),
      ],
    ),
  );
}
