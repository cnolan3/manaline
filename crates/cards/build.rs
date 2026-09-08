fn main() {
    // `deck_dirs` bakes the system data directory in from this variable at
    // compile time; without this line cargo would keep a stale value after
    // it changed, because nothing in the source tree itself did.
    println!("cargo:rerun-if-env-changed=MANALINE_DATA_DIR");
}
