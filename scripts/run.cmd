@echo off
rem build and run tc for Windows; no cd, so tc opens in the caller's directory
cargo run --profile fast --manifest-path "%~dp0..\Cargo.toml" -p tc-gui -- %*
exit /b %errorlevel%
