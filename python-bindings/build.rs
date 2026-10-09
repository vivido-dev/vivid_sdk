// Rust guideline compliant 2026-10-07

fn main() {
    // pyo3 leaves `-undefined dynamic_lookup` to the build frontend (maturin,
    // setuptools-rust), so a bare `cargo build` of the cdylib fails to resolve
    // the interpreter's symbols. Emitting the platform-appropriate extension
    // module link args here makes the crate build out of the box everywhere.
    pyo3_build_config::add_extension_module_link_args();
}
