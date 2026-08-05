# Contributing to voxtype

Thanks for helping make free, open-source dictation better for Linux users!
All types of contributions are welcome: code, docs, packaging, translations,
bug reports, and feature ideas.

## Quick start

```bash
git clone https://github.com/atheerium/voxtype.git
cd voxtype
cargo build --release
cargo test
```

## Development loop

- **Format:** `cargo fmt` (CI runs `cargo fmt --check`)
- **Lint:** `cargo clippy --all-targets -- -D warnings` (CI enforces zero warnings)
- **Tests:** `cargo test` (unit tests live in `src/`)
- **Build:** `cargo build --release` (~2.4 MB binary)

## Project layout

| Path | Purpose |
|---|---|
| `src/main.rs` | CLI entry: `--daemon` flag, SIGUSR1 toggle |
| `src/config.rs` | Config loading, API key resolution |
| `src/dictation.rs` | Daemon: env detection, recording, transcription, paste |
| `install.sh` | One-command installer (kept in sync with README) |
| `.github/workflows/` | CI and release pipelines |

## Making changes

1. **Keep changes focused.** Prefer small, reviewable PRs.
2. **Test your changes.** `cargo test` must pass; add tests for new logic
   (pure helpers are easy to unit test — see the existing `#[cfg(test)]` modules).
3. **Keep it lint-clean.** `cargo clippy --all-targets -- -D warnings` must pass.
4. **Document user-facing changes.** Update `README.md` and `CLAUDE.md`
   (agent docs) when behavior changes.
5. **If you touch `install.sh`**, run `shellcheck install.sh` and
   `bash -n install.sh`, and re-test with `--dry-run`.

## Feature ideas (good first contributions)

- Voice editing commands (select/delete/replace)
- Offline transcription via local Whisper
- Provider abstraction for any OpenAI-compatible API
- System tray indicator
- Packaging: deb, rpm, AUR, Nix, Homebrew
- Test coverage for more of `dictation.rs`

## Reporting issues

Include: your distro, display server (X11/Wayland) and compositor, voxtype
version (`voxtype --version` if supported), and the relevant lines from
`~/.local/share/voxtype/daemon.log`.

## License

By contributing, you agree that your contributions are licensed under the
[MIT license](LICENSE).
