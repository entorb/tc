@echo off
rem build and run tc-gui (desktop) for Windows; no cd, so tc-gui opens in the caller's directory
cargo run --release --quiet --manifest-path "%~dp0..\Cargo.toml" -p tc-gui -- %*
exit /b %errorlevel%
