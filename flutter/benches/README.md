# Focused engine benchmarks

`benches/{parse,style,layout,render}` remain planned as focused in-process engine
measurements as each production capability arrives. No Criterion harness or
accepted 10% regression budget currently exists.

The current dependency-light, process-level measurement foundation lives in
`scripts/` and is documented in `docs/BASELINES.md`. It measures committed local
headless scenarios, Linux process memory, profile growth, and artifact size; it
does not replace future subsystem-level benches.
