// Vixen GUI binary entry point. Intentionally tiny: the entire shell lives
// in the `vixen-shell` crate (ADR-010 — idiomatic Relm4 shell).
//
// When built without the `vixen-shell/gtk-shell` feature (the default in
// environments without the GNOME SDK), `run()` reports that the GTK shell
// is unavailable and exits. With the feature on, it launches the libadwaita
// browser window. See `docs/PLAN.md` Phase 0 and `docs/ARCHITECTURE.md`.

fn main() {
    vixen_shell::run();
}
