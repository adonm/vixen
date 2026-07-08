# GNOME application data

Landed at Phase 0 (docs/PLAN.md) as a skeleton. Populated as the shell
matures.

App IDs (docs/ARCHITECTURE.md "App ID and profile paths"):

- `org.vixen.Vixen`        — production
- `org.vixen.Vixen.Devel`  — development

Shipped contents:

```
org.vixen.Vixen.desktop
org.vixen.Vixen.metainfo.xml
icons/hicolor/scalable/apps/org.vixen.Vixen.svg
```

`org.vixen.Vixen.gschema.xml` lands when persisted preferences are wired.
