check:
  cargo +nightly fmt --check
  cargo clippy
  cargo check

fix:
  cargo +nightly fmt
  cargo clippy --fix

test:
  cargo test

build:
  cargo build --release

install: build
  install target/release/jjscope ~/.local/bin/jjscope
