@echo off
rem build and run tc-cli for Windows; no cd, so tc-cli opens in the caller's directory
cargo run --release --quiet --manifest-path "%~dp0..\Cargo.toml" --bin tc-cli -- %*
exit /b %errorlevel%
