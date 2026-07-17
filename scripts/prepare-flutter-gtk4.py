#!/usr/bin/env python3
"""Remove GTK3-only Yaru plugins from Vixen's generated Linux host files."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

GTK3_PLUGINS = {
    "gtk",
    "screen_retriever_linux",
    "window_manager",
    "yaru_window_linux",
}

REGISTRANT = """//
//  Generated file. Do not edit.
//

// clang-format off

#include \"generated_plugin_registrant.h\"

void fl_register_plugins(FlPluginRegistry* registry) { (void)registry; }
"""

CMAKE = """#
# Generated file, do not edit.
#

list(APPEND FLUTTER_PLUGIN_LIST
)

list(APPEND FLUTTER_FFI_PLUGIN_LIST
)

set(PLUGIN_BUNDLED_LIBRARIES)

foreach(plugin ${FLUTTER_PLUGIN_LIST})
  add_subdirectory(flutter/ephemeral/.plugin_symlinks/${plugin}/linux plugins/${plugin})
  target_link_libraries(${BINARY_NAME} PRIVATE ${plugin}_plugin)
  list(APPEND PLUGIN_BUNDLED_LIBRARIES $<TARGET_FILE:${plugin}_plugin>)
  list(APPEND PLUGIN_BUNDLED_LIBRARIES ${${plugin}_bundled_libraries})
endforeach(plugin)

foreach(ffi_plugin ${FLUTTER_FFI_PLUGIN_LIST})
  add_subdirectory(flutter/ephemeral/.plugin_symlinks/${ffi_plugin}/linux plugins/${ffi_plugin})
  list(APPEND PLUGIN_BUNDLED_LIBRARIES ${${ffi_plugin}_bundled_libraries})
endforeach(ffi_plugin)
"""


def prepare(project: Path) -> None:
    metadata_path = project / ".flutter-plugins-dependencies"
    metadata = json.loads(metadata_path.read_text())
    linux_plugins = metadata["plugins"]["linux"]
    names = {plugin["name"] for plugin in linux_plugins}
    if names != GTK3_PLUGINS:
        raise SystemExit(
            "GTK4 plugin audit is stale: "
            f"expected {sorted(GTK3_PLUGINS)}, found {sorted(names)}"
        )
    metadata["plugins"]["linux"] = []
    metadata_path.write_text(json.dumps(metadata, separators=(",", ":")) + "\n")

    managed = project / "linux" / "flutter"
    (managed / "generated_plugin_registrant.cc").write_text(REGISTRANT)
    (managed / "generated_plugins.cmake").write_text(CMAKE)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("project", type=Path)
    args = parser.parse_args()
    prepare(args.project.resolve())


if __name__ == "__main__":
    main()
