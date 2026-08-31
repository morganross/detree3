# Contributing

Thanks for helping improve DeTree.

1. Open an issue for substantial behavior or format changes.
2. Create a focused branch from `master`.
3. Add or update tests for every behavior change.
4. Run the complete local checks:

   ```bash
   cargo fmt --check
   cargo test --locked
   cargo clippy --all-targets --all-features -- -D warnings
   ```

5. Submit a pull request describing the user-visible effect and compatibility
   considerations.

Please keep DeTree small. New dependencies should have a clear user-facing
benefit and should not duplicate straightforward standard-library behavior.

