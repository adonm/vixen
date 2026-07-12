# Artifact-size controls

`flutter_hello/` is the checked-in Flutter 3.44 Linux project used as Vixen's
like-for-like release-bundle size peer. It uses Material, the generated Linux
runner, the same pinned Flutter framework revision, and no Vixen code or native
library. It is a build input and is never shipped.

Run `just size-flutter-linux` to build both release/AOT bundles without network
access and compare them. The resulting report measures relocatable bundle files;
it is not Flatpak, compressed-download, installed-size, or accepted-budget
evidence.
