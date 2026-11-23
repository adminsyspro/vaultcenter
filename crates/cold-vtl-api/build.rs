// sqlx::migrate! embeds ./migrations at compile time; rebuild when one is added.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
    // build_info reads the revision with option_env!, so a new value must recompile.
    println!("cargo:rerun-if-env-changed=VAULTCENTER_REVISION");
}
