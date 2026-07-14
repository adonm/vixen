import 'dart:convert';

import 'package:vixen_shell/src/bridge/render_models.dart';

const _png =
    'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAF0lEQVR4nGP4z8Dw'
    'HwwZGP7///+f4T8AR8oI+P1do8oAAAAASUVORK5CYII=';

RenderRevision r3Revision(int generation) => RenderRevision(
  contextId: 1,
  documentId: 2,
  sourceGeneration: generation,
  styleGeneration: generation,
  viewportGeneration: 1,
  resourceGeneration: 1,
);

FullRenderSnapshot r3Snapshot({int generation = 1, bool updated = false}) {
  final nodes = <RenderNode>[
    RenderNode(
      id: 1,
      parentId: null,
      siblingIndex: 0,
      depth: 0,
      kind: RenderNodeKind.element,
      name: 'html',
      styles: {'background': '#f0f4f8', 'height': '208'},
    ),
    RenderNode(
      id: 2,
      parentId: 1,
      siblingIndex: 0,
      depth: 1,
      kind: RenderNodeKind.element,
      name: 'article',
      styles: {
        'margin': '12',
        'padding': '12',
        'background': updated ? '#34506f' : '#203040',
      },
    ),
    RenderNode(
      id: 3,
      parentId: 2,
      siblingIndex: 0,
      depth: 2,
      kind: RenderNodeKind.element,
      name: 'h1',
      semantic: RenderSemanticDescriptor(
        id: 1,
        role: 'heading',
        name: updated ? 'Updated Vixen' : 'Vixen renderer',
        actionGeneration: generation,
      ),
    ),
    RenderNode(
      id: 4,
      parentId: 3,
      siblingIndex: 0,
      depth: 3,
      kind: RenderNodeKind.text,
      name: '#text',
      text: updated ? 'Updated Vixen' : 'Vixen renderer',
      styles: const {
        'font-size': '22',
        'font-weight': 'bold',
        'color': '#ffffff',
      },
    ),
    RenderNode(
      id: 5,
      parentId: 2,
      siblingIndex: 2,
      depth: 2,
      kind: RenderNodeKind.element,
      name: 'p',
      styles: const {'margin': '4'},
      semantic: RenderSemanticDescriptor(
        id: 2,
        role: 'text',
        name: 'Styled wrapped body text',
        actionGeneration: generation,
      ),
    ),
    RenderNode(
      id: 6,
      parentId: 5,
      siblingIndex: 0,
      depth: 3,
      kind: RenderNodeKind.text,
      name: '#text',
      text: 'Flutter Paragraph owns wrapping and exact UTF-16 range geometry for this controlled document.',
      styles: const {'font-size': '15', 'color': '#ffd166'},
    ),
    RenderNode(
      id: 7,
      parentId: 2,
      siblingIndex: 3,
      depth: 2,
      kind: RenderNodeKind.element,
      name: 'a',
      styles: const {'margin': '4'},
      semantic: RenderSemanticDescriptor(
        id: 3,
        role: 'link',
        name: 'Read more',
        actionGeneration: generation,
      ),
    ),
    RenderNode(
      id: 8,
      parentId: 7,
      siblingIndex: 0,
      depth: 3,
      kind: RenderNodeKind.text,
      name: '#text',
      text: 'Read more',
      styles: const {'font-size': '15', 'color': '#70d6ff'},
    ),
    RenderNode(
      id: 10,
      parentId: 5,
      siblingIndex: 1,
      depth: 3,
      kind: RenderNodeKind.text,
      name: '#text',
      text: ' Mixed style.',
      styles: const {
        'font-size': '17',
        'font-weight': 'bold',
        'color': '#ff8fab',
      },
    ),
    RenderNode(
      id: 11,
      parentId: 5,
      siblingIndex: 2,
      depth: 3,
      kind: RenderNodeKind.text,
      name: '#text',
      text: ' Final run.',
      styles: const {'font-size': '13', 'color': '#b8f2e6'},
    ),
    RenderNode(
      id: 9,
      parentId: 2,
      siblingIndex: 1,
      depth: 2,
      kind: RenderNodeKind.element,
      name: 'img',
      styles: const {'width': '32', 'margin': '4'},
      resourceIds: const [1],
    ),
  ];
  return FullRenderSnapshot(
    revision: r3Revision(generation),
    viewport: const RenderViewport(width: 240, height: 160),
    nodes: nodes,
    resources: [
      RenderResource(id: 1, mime: 'image/png', bytes: base64Decode(_png)),
    ],
  );
}

RenderMutationBatch r3Mutation() => RenderMutationBatch(
  baseRevision: r3Revision(1),
  targetRevision: r3Revision(2),
  mutations: [
    UpsertRenderNode(r3Snapshot(generation: 2, updated: true).nodes[1]),
    UpsertRenderNode(r3Snapshot(generation: 2, updated: true).nodes[3]),
    UpsertRenderNode(r3Snapshot(generation: 2, updated: true).nodes[2]),
  ],
);
