# Contributing to ink-notes

**English** · [Deutsch](CONTRIBUTING.de.md)

Thank you for wanting to improve ink-notes!

## Where?

The main repository is on **Gitea**: <https://gitea.pansi.eu/harald/ink-notes>.
[GitHub](https://github.com/haraldpansi-gif/ink-notes) is a mirror – issues and pull requests are welcome there too, but pull requests are applied to Gitea by hand.

## Reporting bugs and suggesting ideas

- Please check first whether there is already an [issue](https://gitea.pansi.eu/harald/ink-notes/issues) for it.
- For bugs, use the "Bug report" template: version (☰ → Updates), distribution, Wayland or X11, input device (pen, finger, mouse, touchpad) and the steps that trigger the bug. A screenshot or a short recording often helps a lot.
- English and German are both fine.
- **Please do not report security vulnerabilities publicly**; follow [SECURITY.md](SECURITY.md) instead.

## Contributing code

1. Discuss larger changes in an issue first.
2. Build and test as described in the [README](README.md#development). These checks must pass before submitting (CI checks them too):
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
3. New features and bug fixes get a test where that makes sense.
4. Document changes to the file format in [docs/FILE_FORMAT.md](docs/FILE_FORMAT.md) (and its German version); older notebooks must remain readable.
5. Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/), e.g. `fix: …`, `feat: …`, `perf: …`, `docs: …`; refer to an issue as `(Issue #28)`. New code comments and commit messages are written in English (older code and history still contain German).
6. The interface is available in German and English: write new texts with `t!("…")` (the German text is the key) and add the English translation in `src/i18n/`.

By contributing you agree that your contribution is licensed under the project's [MIT License](LICENSE). Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
