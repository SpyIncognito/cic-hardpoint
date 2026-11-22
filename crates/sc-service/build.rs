//! The pages are embedded from pages/ at the repository root; rebuild when any of them changes,
//! including a file added to the folder.
fn main() {
    println!("cargo:rerun-if-changed=../../pages");
}
