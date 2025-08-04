cargo fmt

cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --fix --allow-dirty --allow-staged --workspace --all-targets
