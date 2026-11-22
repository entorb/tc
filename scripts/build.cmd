@echo off
rem build release binaries of all workspace crates for Windows into target\release
cargo build --release --locked --workspace --bins --manifest-path "%~dp0..\Cargo.toml"
if errorlevel 1 exit /b %errorlevel%
dir /b "%~dp0..\target\release\tc-*.exe"
exit /b %errorlevel%
