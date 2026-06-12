fn main() {
    // Only emit ESP-IDF link instructions when cross-compiling for the ESP target.
    // On the host target this is a no-op, so host unit tests build with plain cargo.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("espidf") {
        embuild::espidf::sysenv::output();
    }
}
