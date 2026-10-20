# Mitmachen bei ink-notes

[English](CONTRIBUTING.md) · **Deutsch**

Danke, dass du ink-notes verbessern willst!

## Wo?

Das Hauptrepository liegt auf **Gitea**: <https://gitea.pansi.eu/harald/ink-notes>.
[GitHub](https://github.com/haraldpansi-gif/ink-notes) ist ein Spiegel davon – Issues und Pull Requests sind dort auch willkommen, Pull Requests werden aber von Hand nach Gitea übernommen.

## Fehler melden und Ideen vorschlagen

- Bitte zuerst nachsehen, ob es schon ein [Issue](https://gitea.pansi.eu/harald/ink-notes/issues) dazu gibt.
- Für Fehler die Vorlage „Fehler melden“ verwenden: Version (☰ → Updates), Distribution, Wayland oder X11, Eingabegerät (Stift, Finger, Maus, Touchpad) und die Schritte, mit denen der Fehler auftritt. Ein Bildschirmfoto oder eine kurze Aufnahme hilft oft sehr.
- Deutsch und Englisch sind beide in Ordnung.
- **Sicherheitslücken bitte nicht öffentlich melden**, sondern wie in [SECURITY.de.md](SECURITY.de.md) beschrieben.

## Code beitragen

1. Größere Änderungen am besten vorher in einem Issue absprechen.
2. Bauen und testen, wie im [README](README.de.md#entwicklung) beschrieben. Vor dem Einreichen müssen diese Prüfungen durchlaufen (die CI prüft sie ebenfalls):
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
3. Neue Funktionen und behobene Fehler bekommen einen Test, wo das sinnvoll geht.
4. Änderungen am Dateiformat bitte in [docs/FILE_FORMAT.de.md](docs/FILE_FORMAT.de.md) nachtragen; ältere Notizbücher müssen weiter lesbar bleiben.
5. Commit-Nachrichten im Stil von [Conventional Commits](https://www.conventionalcommits.org/), z. B. `fix: …`, `feat: …`, `perf: …`, `docs: …`; ein Bezug auf ein Issue als `(Issue #28)`. Neue Code-Kommentare und Commit-Nachrichten werden auf Englisch geschrieben (älterer Code und die Historie enthalten noch Deutsch).
6. Texte der Oberfläche gibt es auf Deutsch und Englisch: Neue Texte mit `t!("…")` schreiben und die englische Übersetzung unter `src/i18n/` ergänzen.

Mit dem Einreichen stimmst du zu, dass dein Beitrag unter der [MIT-Lizenz](LICENSE) des Projekts steht. Für den Umgang miteinander gilt der [Code of Conduct](CODE_OF_CONDUCT.md).

