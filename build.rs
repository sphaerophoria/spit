fn main() {
    println!("cargo:rerun-if-changed=hooks/pre-push");
    let _ = std::fs::copy("hooks/pre-push", ".git/hooks/pre-push");
}
