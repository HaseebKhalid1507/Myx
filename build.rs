// Minimal build script whose only job is to pull a build-dependency on
// `vergen = "=9.0.6"` into the graph. librespot-core's build script uses vergen
// too; without this pin a fresh `cargo install` resolves vergen 9.1.0, which is
// incompatible with the vergen-lib 0.1.6 that vergen-gitcl requires, breaking
// librespot-core's build. Forcing 9.0.6 unifies the graph. (See librespot #…)
fn main() {}
