// build.rs — FUTURE WORK (intentionally a near no-op for now).
//
// The plan: at build time, run `wyn compile` for each Wyn root and diff the
// emitted `<name>.json` pipeline descriptor against the frame-graph declared in
// `src/app.rs`, failing the build on drift. That keeps the Rust graph (which the
// driver compiles in, and never re-reads at runtime) honest against the shaders.
//
// Not implemented yet — see the project plan's "Open research items".
fn main() {
    // Rebuild if the shaders change, so a future validation step re-runs.
    println!("cargo:rerun-if-changed=../wyn");
    println!("cargo:rerun-if-changed=../shaders");
    // TODO(build-validation): invoke `wyn compile` + diff descriptor vs app.rs graph.
}
