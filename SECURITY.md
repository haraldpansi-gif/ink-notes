# Security Policy

**English** · [Deutsch](SECURITY.de.md)

## Supported versions

Security fixes are only provided for the **latest release** (see [Releases](https://gitea.pansi.eu/harald/ink-notes/releases)). The app offers updates itself (☰ → Updates); the Flatpak is updated through the software manager.

## Reporting a vulnerability

Please do **not** report vulnerabilities in a public issue. Send an e-mail to **harald@pansi.eu** instead, ideally with:

- the affected version and installation type (.deb, AppImage, Flatpak, built from source),
- a description of what can happen,
- steps or a file (e.g. a crafted notebook or PDF) to reproduce it.

You will get an answer within a week. Confirmed issues are fixed in a new release; on request you will be credited in the release notes. Reports in English or German are both fine.

## Areas of interest

- **Updates:** verification of the Minisign signature and SHA-256 checksums, downloading and replacing packages (see "Update security" in the [README](README.md#update-security)).
- **Opening files:** imported PDFs and images, notebook files ([file format](docs/FILE_FORMAT.md)) – e.g. crashes or writes outside the data folder caused by crafted files.
