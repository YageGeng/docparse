use docparse_core::DocParser;

/// Fails at compile time if browser support weakens the native parser's thread contract.
#[test]
fn native_parser_remains_send_and_sync() {
    /// Requires the public type to retain both auto traits.
    fn require<T: Send + Sync>() {}
    require::<DocParser>();
}
