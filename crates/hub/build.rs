// Recompile when the built frontend changes: rust-embed scans web/dist at
// compile time, so a binary built before `npm run build` would serve stale
// or missing files.
fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
}
