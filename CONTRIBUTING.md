# Mitmachen bei ink-notes

Danke, dass du ink-notes verbessern willst! *(English below.)*

## Wo?

Das Hauptrepository liegt auf **Gitea**: <https://gitea.pansi.eu/harald/ink-notes>.
[GitHub](https://github.com/haraldpansi-gif/ink-notes) ist ein Spiegel davon – Issues und Pull Requests sind dort auch willkommen, Pull Requests werden aber von Hand nach Gitea übernommen.

## Fehler melden und Ideen vorschlagen

- Bitte zuerst nachsehen, ob es schon ein [Issue](https://gitea.pansi.eu/harald/ink-notes/issues) dazu gibt.
- Für Fehler die Vorlage „Fehler melden“ verwenden: Version (☰ → Updates), Distribution, Wayland oder X11, Eingabegerät (Stift, Finger, Maus, Touchpad) und die Schritte, mit denen der Fehler auftritt. Ein Bildschirmfoto oder eine kurze Aufnahme hilft oft sehr.
- Deutsch und Englisch sind beide in Ordnung.
- **Sicherheitslücken bitte nicht öffentlich melden**, sondern wie in [SECURITY.md](SECURITY.md) beschrieben.

## Code beitragen

1. Größere Änderungen am besten vorher in einem Issue absprechen.
2. Bauen und testen, wie im [README](README.de.md#entwicklung) beschrieben. Vor dem Einreichen müssen diese Prüfungen durchlaufen (die CI prüft sie ebenfalls):
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
3. Neue Funktionen und behobene Fehler bekommen einen Test, wo das sinnvoll geht.
4. Änderungen am Dateiformat bitte in [docs/FILE_FORMAT.md](docs/FILE_FORMAT.md) nachtragen; ältere Notizbücher müssen weiter lesbar bleiben.
5. Commit-Nachrichten im Stil von [Conventional Commits](https://www.conventionalcommits.org/), z. B. `fix: …`, `feat: …`, `perf: …`, `docs: …`; ein Bezug auf ein Issue als `(Issue #28)`.
6. Texte der Oberfläche gibt es auf Deutsch und Englisch: Neue Texte mit `t!("…")` schreiben und die englische Übersetzung unter `src/i18n/` ergänzen.

Mit dem Einreichen stimmst du zu, dass dein Beitrag unter der [MIT-Lizenz](LICENSE) des Projekts steht. Für den Umgang miteinander gilt der [Code of Conduct](CODE_OF_CONDUCT.md).

---

## Contributing (English)

The main repository is on **Gitea**: <https://gitea.pansi.eu/harald/ink-notes>. [GitHub](https://github.com/haraldpansi-gif/ink-notes) is a mirror; issues and pull requests are welcome there too, but pull requests are applied to Gitea by hand.

- **Bugs and ideas:** check the [existing issues](https://gitea.pansi.eu/harald/ink-notes/issues) first, then use the bug report or feature request template. Please include the version (☰ → Updates), distribution, Wayland or X11, the input device and the steps to reproduce. German and English are both fine.
- **Security issues:** please do not report them publicly; see [SECURITY.md](SECURITY.md).
- **Code:** discuss larger changes in an issue first. Before submitting, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass. Add tests where it makes sense, document file format changes in `docs/FILE_FORMAT.md`, use Conventional Commits, and add English translations for new UI texts (`t!("…")`, see `src/i18n/`).

By contributing you agree that your contribution is licensed under the project's [MIT License](LICENSE). Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
