@echo off
rem build and run tc-ui (desktop) for Windows; no cd, so tc-ui opens in the caller's directory
cargo run --release --quiet --manifest-path "%~dp0..\Cargo.toml" -p tc-ui -- %*
exit /b %errorlevel%
