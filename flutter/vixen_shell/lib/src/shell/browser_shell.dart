import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:yaru/yaru.dart';

import '../bridge/browser_models.dart';
import 'shell_coordinator.dart';
import 'content_surface.dart';

export 'content_surface.dart' show BrowserContentSurface;

final class BrowserShell extends StatefulWidget {
  const BrowserShell({required this.coordinator, super.key});

  final ShellCoordinator coordinator;

  @override
  State<BrowserShell> createState() => _BrowserShellState();
}

final class _BrowserShellState extends State<BrowserShell>
    with WidgetsBindingObserver {
  final TextEditingController _addressController = TextEditingController();
  final TextEditingController _findController = TextEditingController();
  final FocusNode _addressFocus = FocusNode();
  final FocusNode _findFocus = FocusNode();
  bool _findVisible = false;
  BrowserHostLifecycle _hostLifecycle = BrowserHostLifecycle.resumed;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
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
    WidgetsBinding.instance.removeObserver(this);
    widget.coordinator.removeListener(_coordinatorChanged);
    _addressController.dispose();
    _findController.dispose();
    _addressFocus.dispose();
    _findFocus.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final lifecycle = switch (state) {
      AppLifecycleState.resumed => BrowserHostLifecycle.resumed,
      AppLifecycleState.inactive => BrowserHostLifecycle.inactive,
      AppLifecycleState.hidden => BrowserHostLifecycle.hidden,
      AppLifecycleState.paused => BrowserHostLifecycle.paused,
      AppLifecycleState.detached => BrowserHostLifecycle.detached,
    };
    if (_hostLifecycle != lifecycle && mounted) {
      setState(() => _hostLifecycle = lifecycle);
    }
    widget.coordinator.updateApplicationLifecycle(lifecycle);
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
        const SingleActivator(LogicalKeyboardKey.keyF, control: true):
            _showFind,
        const SingleActivator(LogicalKeyboardKey.f3): _findNext,
        const SingleActivator(LogicalKeyboardKey.f3, shift: true):
            _findPrevious,
        const SingleActivator(LogicalKeyboardKey.equal, control: true): () {
          unawaited(coordinator.zoomIn());
        },
        const SingleActivator(LogicalKeyboardKey.minus, control: true): () {
          unawaited(coordinator.zoomOut());
        },
        const SingleActivator(LogicalKeyboardKey.digit0, control: true): () {
          unawaited(coordinator.resetZoom());
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
          appBar: BrowserWindowTitleBar(coordinator: coordinator),
          body: SafeArea(
            top: false,
            child: Column(
              children: [
                if (coordinator.errorMessage case final message?)
                  _ErrorBanner(
                    message: message,
                    onDismiss: coordinator.clearError,
                  ),
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
                  onShowFind: _showFind,
                ),
                if (_findVisible)
                  _FindBar(
                    controller: _findController,
                    focusNode: _findFocus,
                    matches: coordinator.findMatches,
                    activeMatch: coordinator.findActiveMatch,
                    onChanged: (query) {
                      unawaited(coordinator.findText(query));
                    },
                    onNext: _findNext,
                    onPrevious: _findPrevious,
                    onClose: _closeFind,
                  ),
                _SelectedProgress(context: coordinator.selectedContext),
                Expanded(
                  child: BrowserContentSurface(
                    contextState: coordinator.selectedContext,
                    rendererView: coordinator.rendererView,
                    rendererFindResult: coordinator.rendererFindResult,
                    onRendererPresented: (view) {
                      unawaited(coordinator.rendererCommitPresented(view));
                    },
                    onRendererSemanticAction:
                        (view, descriptor, action, value) {
                          unawaited(
                            coordinator.dispatchRendererSemanticAction(
                              view,
                              descriptor,
                              action,
                              value,
                            ),
                          );
                        },
                    lifecycle: _hostLifecycle,
                    accessibility: coordinator.accessibility,
                    onPhysicalViewportChanged:
                        coordinator.updatePhysicalViewport,
                    onFocusChanged: coordinator.updateContentFocus,
                    onMouseEvent: (eventType, event) {
                      unawaited(
                        coordinator.dispatchMouseEvent(eventType, event),
                      );
                    },
                    onKeyEvent: (eventType, event) {
                      unawaited(coordinator.dispatchKeyEvent(eventType, event));
                    },
                    onTextInput: (state) {
                      unawaited(coordinator.dispatchTextInput(state));
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
    if (_findVisible) {
      _closeFind();
    } else if (_addressFocus.hasFocus) {
      final url = widget.coordinator.selectedContext?.url ?? '';
      _addressController.text = url;
      _addressFocus.unfocus();
    } else {
      unawaited(widget.coordinator.stop());
    }
  }

  void _showFind() {
    if (!_findVisible) setState(() => _findVisible = true);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || !_findVisible) return;
      _findFocus.requestFocus();
      _findController.selection = TextSelection(
        baseOffset: 0,
        extentOffset: _findController.text.length,
      );
    });
  }

  void _closeFind() {
    if (!_findVisible) return;
    setState(() => _findVisible = false);
    _findController.clear();
    _findFocus.unfocus();
    unawaited(widget.coordinator.findText(''));
  }

  void _findNext() {
    if (!_findVisible) {
      _showFind();
      return;
    }
    unawaited(widget.coordinator.findText(_findController.text));
  }

  void _findPrevious() {
    if (!_findVisible) {
      _showFind();
      return;
    }
    unawaited(
      widget.coordinator.findText(_findController.text, forward: false),
    );
  }
}

final class BrowserWindowTitleBar extends StatelessWidget
    implements PreferredSizeWidget {
  const BrowserWindowTitleBar({required this.coordinator, super.key});

  final ShellCoordinator coordinator;

  @override
  Size get preferredSize => const Size.fromHeight(kYaruTitleBarHeight);

  @override
  Widget build(BuildContext context) {
    return YaruWindowTitleBar(
      key: const Key('window-title-bar'),
      style: YaruTitleBarStyle.normal,
      centerTitle: false,
      titleSpacing: 0,
      closeSemanticLabel: 'Close Vixen',
      maximizeSemanticLabel: 'Maximize Vixen',
      minimizeSemanticLabel: 'Minimize Vixen',
      restoreSemanticLabel: 'Restore Vixen',
      title: _TabStrip(coordinator: coordinator),
    );
  }
}

final class _TabStrip extends StatelessWidget {
  const _TabStrip({required this.coordinator});

  final ShellCoordinator coordinator;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return SizedBox(
      height: kYaruTitleBarHeight,
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
                  child: Padding(
                    padding: const EdgeInsets.symmetric(vertical: 5),
                    child: InkWell(
                      key: ValueKey('tab-${tab.contextId}'),
                      borderRadius: BorderRadius.circular(kYaruButtonRadius),
                      onTap: () =>
                          unawaited(coordinator.activateTab(tab.contextId)),
                      child: AnimatedContainer(
                        duration: const Duration(milliseconds: 100),
                        width: 200,
                        padding: const EdgeInsets.only(left: 12),
                        decoration: BoxDecoration(
                          color: selected
                              ? colors.onSurface.withValues(alpha: 0.1)
                              : Colors.transparent,
                          borderRadius: BorderRadius.circular(
                            kYaruButtonRadius,
                          ),
                        ),
                        child: Row(
                          children: [
                            if (tab.isLoading)
                              const SizedBox.square(
                                dimension: 16,
                                child: YaruCircularProgressIndicator(
                                  strokeWidth: 2,
                                ),
                              )
                            else
                              const Icon(YaruIcons.globe, size: 16),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                tab.displayTitle,
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                            ),
                            YaruIconButton(
                              key: ValueKey('close-tab-${tab.contextId}'),
                              tooltip: 'Close tab',
                              iconSize: 30,
                              icon: const Icon(
                                YaruIcons.window_close,
                                size: 16,
                              ),
                              onPressed: () => unawaited(
                                coordinator.closeTab(tab.contextId),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                );
              },
            ),
          ),
          YaruIconButton(
            key: const Key('new-tab'),
            tooltip: 'New tab',
            iconSize: 36,
            onPressed: () => unawaited(coordinator.newTab()),
            icon: const Icon(YaruIcons.tab_new, size: 20),
          ),
          const SizedBox(width: 6),
        ],
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
    required this.onShowFind,
  });

  final ShellCoordinator coordinator;
  final TextEditingController addressController;
  final FocusNode addressFocus;
  final ValueChanged<String> onSubmitted;
  final VoidCallback onShowAbout;
  final VoidCallback onShowShortcuts;
  final VoidCallback onShowFind;

  @override
  Widget build(BuildContext context) {
    final tab = coordinator.selectedContext;
    return Material(
      elevation: 1,
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 7),
        child: Row(
          children: [
            YaruIconButton(
              key: const Key('back'),
              tooltip: 'Back (Alt+Left)',
              iconSize: 40,
              onPressed: tab?.canGoBack == true
                  ? () => unawaited(coordinator.goBack())
                  : null,
              icon: const Icon(YaruIcons.go_previous),
            ),
            YaruIconButton(
              key: const Key('forward'),
              tooltip: 'Forward (Alt+Right)',
              iconSize: 40,
              onPressed: tab?.canGoForward == true
                  ? () => unawaited(coordinator.goForward())
                  : null,
              icon: const Icon(YaruIcons.go_next),
            ),
            YaruIconButton(
              key: const Key('reload-stop'),
              tooltip: tab?.isLoading == true
                  ? 'Stop (Escape)'
                  : 'Reload (Ctrl+R)',
              iconSize: 40,
              onPressed: tab == null
                  ? null
                  : () => unawaited(
                      tab.isLoading ? coordinator.stop() : coordinator.reload(),
                    ),
              icon: Icon(
                tab?.isLoading == true ? YaruIcons.stop : YaruIcons.refresh,
              ),
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
                  prefixIcon: const Icon(YaruIcons.globe, size: 19),
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
            YaruPopupMenuButton<_MenuAction>(
              key: const Key('main-menu'),
              tooltip: 'Vixen menu',
              showArrow: false,
              child: const Icon(YaruIcons.menu),
              onSelected: (action) {
                switch (action) {
                  case _MenuAction.find:
                    onShowFind();
                  case _MenuAction.zoomIn:
                    unawaited(coordinator.zoomIn());
                  case _MenuAction.zoomOut:
                    unawaited(coordinator.zoomOut());
                  case _MenuAction.resetZoom:
                    unawaited(coordinator.resetZoom());
                  case _MenuAction.shortcuts:
                    onShowShortcuts();
                  case _MenuAction.about:
                    onShowAbout();
                }
              },
              itemBuilder: (context) => const [
                PopupMenuItem(
                  value: _MenuAction.find,
                  child: Text('Find in page'),
                ),
                PopupMenuItem(
                  value: _MenuAction.zoomIn,
                  child: Text('Zoom in'),
                ),
                PopupMenuItem(
                  value: _MenuAction.zoomOut,
                  child: Text('Zoom out'),
                ),
                PopupMenuItem(
                  value: _MenuAction.resetZoom,
                  child: Text('Reset zoom'),
                ),
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

enum _MenuAction { find, zoomIn, zoomOut, resetZoom, shortcuts, about }

final class _FindBar extends StatelessWidget {
  const _FindBar({
    required this.controller,
    required this.focusNode,
    required this.matches,
    required this.activeMatch,
    required this.onChanged,
    required this.onNext,
    required this.onPrevious,
    required this.onClose,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final int? matches;
  final int? activeMatch;
  final ValueChanged<String> onChanged;
  final VoidCallback onNext;
  final VoidCallback onPrevious;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final query = controller.text;
    final result = query.isEmpty
        ? 'Type to find'
        : matches == null
        ? 'Searching…'
        : matches == 0
        ? '0 matches'
        : '$activeMatch of $matches';
    return Material(
      key: const Key('find-bar'),
      color: Theme.of(context).colorScheme.surfaceContainerLow,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 6, 8, 6),
        child: Row(
          children: [
            Expanded(
              child: TextField(
                key: const Key('find-field'),
                controller: controller,
                focusNode: focusNode,
                maxLength: 4096,
                onChanged: onChanged,
                onSubmitted: (_) => onNext(),
                textInputAction: TextInputAction.search,
                decoration: const InputDecoration(
                  hintText: 'Find in page',
                  isDense: true,
                  counterText: '',
                ),
              ),
            ),
            const SizedBox(width: 12),
            Semantics(
              liveRegion: true,
              child: Text(result, key: const Key('find-result')),
            ),
            YaruIconButton(
              key: const Key('find-previous'),
              tooltip: 'Previous match (Shift+F3)',
              iconSize: 40,
              onPressed: matches != null && matches! > 0 ? onPrevious : null,
              icon: const Icon(YaruIcons.pan_up),
            ),
            YaruIconButton(
              key: const Key('find-next'),
              tooltip: 'Next match (F3)',
              iconSize: 40,
              onPressed: matches != null && matches! > 0 ? onNext : null,
              icon: const Icon(YaruIcons.pan_down),
            ),
            YaruIconButton(
              tooltip: 'Close find',
              iconSize: 40,
              onPressed: onClose,
              icon: const Icon(YaruIcons.window_close),
            ),
          ],
        ),
      ),
    );
  }
}

final class _SelectedProgress extends StatelessWidget {
  const _SelectedProgress({required this.context});
  final BrowsingContextState? context;

  @override
  Widget build(BuildContext context) {
    final state = this.context;
    if (state?.isLoading != true) return const SizedBox(height: 2);
    final progress = state!.loadProgress;
    return YaruLinearProgressIndicator(
      key: const Key('page-progress'),
      strokeWidth: 2,
      trackStrokeWidth: 2,
      value: progress > 0 && progress <= 1 ? progress : null,
    );
  }
}

final class _StatusBar extends StatelessWidget {
  const _StatusBar({required this.status});
  final String status;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      key: const Key('browser-status-semantics'),
      container: true,
      liveRegion: true,
      label: 'Browser status|$status',
      excludeSemantics: true,
      child: Container(
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
    return SizedBox(
      height: 52,
      child: YaruBanner(
        key: const Key('error-banner'),
        color: colors.errorContainer,
        elevation: 0,
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
        child: Row(
          children: [
            Icon(Icons.error_outline, color: colors.onErrorContainer),
            const SizedBox(width: 12),
            Expanded(child: Text(message)),
            YaruIconButton(
              tooltip: 'Dismiss error',
              iconSize: 36,
              onPressed: onDismiss,
              icon: const Icon(YaruIcons.window_close),
            ),
          ],
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
        'Ctrl+F  Find in page\n'
        'Ctrl++ / Ctrl+- / Ctrl+0  Zoom\n'
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
