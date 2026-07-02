// Desktop entry so `cargo run -p pulsar-mobile` boots the same app for quick
// sanity checks. On Android/iOS the OS loads the cdylib and calls `run()` via
// the `mobile_entry_point` export instead, so this `main` is never used there.
fn main() {
    pulsar_mobile_lib::run()
}
