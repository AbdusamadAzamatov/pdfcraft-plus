# Installing PdfCraft Plus on macOS

PdfCraft Plus is a personal fork of [PdfCraft](https://github.com/storytold/pdfcraft). This page
explains how to install the ready-made macOS build from
[GitHub Releases](https://github.com/AbdusamadAzamatov/pdfcraft-plus/releases), what to do about
the security warning on first launch and how to build the app yourself.

## Requirements

- macOS 11 Big Sur or later.
- An Apple silicon (M-series) or Intel Mac. The download is one universal app that runs natively
  on both.

## What each release contains

| File | What it is |
|---|---|
| `pdfcraft-plus-<version>-macos-universal.dmg` | The app, **PdfCraft Plus.app**, on a disk image |
| `pdfcraft-plus-cli-<version>-macos-universal.zip` | The command-line tool `pdfcraft-cli`, with its licence files |
| `SHA256SUMS.txt` | A SHA-256 checksum for each of the two files above |

`<version>` is the release number, for example `0.14.0`.

## Install the app

1. Open the [latest release](https://github.com/AbdusamadAzamatov/pdfcraft-plus/releases/latest)
   and download `pdfcraft-plus-<version>-macos-universal.dmg` (and `SHA256SUMS.txt` if you want to
   verify it, see below).
2. Double-click the DMG. A window opens showing **PdfCraft Plus** and an **Applications** folder.
3. Drag **PdfCraft Plus** onto **Applications** and wait for the copy to finish.
4. Eject the disk image (the eject button next to "PdfCraft Plus" in the Finder sidebar) and, if
   you like, delete the DMG.

Run the app from the Applications folder, not from inside the disk image, which is read-only.

## First launch: "Apple could not verify…"

PdfCraft Plus is signed ad hoc and is **not notarised** by Apple, because the fork does not have a
paid Apple Developer account yet. Gatekeeper therefore blocks the first launch with a message such
as *"Apple could not verify 'PdfCraft Plus' is free of malware that may harm your Mac or compromise
your privacy"* or *"macOS cannot verify that this app is free from malware"*. The wording varies
between macOS versions. Nothing is wrong with the download; macOS just has no notarisation ticket
to check.

To allow it, once per installed version:

1. Open PdfCraft Plus from Applications. When the warning appears, click **Done** (or **OK**). Do
   not click **Move to Bin**.
2. Open **System Settings › Privacy & Security** (on macOS 12 and earlier: **System Preferences ›
   Security & Privacy › General**).
3. Scroll down to **Security**. You will see a line saying PdfCraft Plus was blocked. Click
   **Open Anyway**. The button is only there for about an hour after the blocked launch.
4. Confirm with **Open Anyway** or **Open** in the dialog that follows and enter your login
   password if asked.

PdfCraft Plus then opens, and macOS remembers your choice for this version. Apple describes the
same steps in
[Open a Mac app from an unknown developer](https://support.apple.com/en-us/102445).

On macOS 14 Sonoma and earlier, right-clicking (or Control-clicking) the app in Applications and
choosing **Open** also works. macOS 15 Sequoia removed that shortcut, so use System Settings there.

### If macOS says the app "is damaged and can't be opened"

That message usually means the download was incomplete or altered. Delete the app, download the
DMG again and check its checksum (below). If the checksum matches and the message stays, you can
remove the quarantine flag that the browser added to the copy in Applications:

```sh
xattr -dr com.apple.quarantine "/Applications/PdfCraft Plus.app"
```

Only do this for a file whose checksum you have verified. The quotes are needed because the name
contains a space.

## Verify the download

`SHA256SUMS.txt` lists a SHA-256 checksum for every file of the release. Put it in the same folder
as the files you downloaded and run, in Terminal:

```sh
cd ~/Downloads
shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

Each file you downloaded should be listed with `OK`. `--ignore-missing` skips files you did not
download (for example the CLI zip). A `FAILED` line means the file is not the one that was
published: delete it and download it again.

## The command-line tool

Unzip `pdfcraft-plus-cli-<version>-macos-universal.zip` and move `pdfcraft-cli` somewhere on your
`PATH`, for example:

```sh
cd ~/Downloads
ditto -x -k pdfcraft-plus-cli-<version>-macos-universal.zip .
sudo mkdir -p /usr/local/bin
sudo install -m 755 pdfcraft-plus-cli-<version>-macos-universal/pdfcraft-cli /usr/local/bin/
pdfcraft-cli --version
```

The tool is also not notarised. If macOS blocks it, allow it under **System Settings › Privacy &
Security** as for the app, or, after verifying the zip's checksum, remove the quarantine flag (if
the command says there is no such attribute, the file was not quarantined):

```sh
xattr -d com.apple.quarantine /usr/local/bin/pdfcraft-cli
```

## Settings and upstream PdfCraft

PdfCraft Plus has its own bundle identifier (`io.github.abdusamadazamatov.pdfcraft-plus`) and its
own name on disk (`PdfCraft Plus.app`), so it can be installed next to upstream PdfCraft
(`PdfCraft.app`). Its settings and file associations are kept separate from upstream's: changing
one app's preferences does not change the other's.

To make PdfCraft Plus the default app for PDFs, select any PDF in Finder, choose **File › Get
Info**, pick PdfCraft Plus under **Open with** and click **Change All…**.

## Update

Download the new DMG from Releases and drag PdfCraft Plus onto Applications again, choosing
**Replace**. Your settings are kept. Because each release is a new build, macOS asks you to allow
it once more (see [First launch](#first-launch-apple-could-not-verify)).

## Uninstall

Quit PdfCraft Plus and move `/Applications/PdfCraft Plus.app` to the Bin. Its settings stay in your
user Library; to remove those too, delete the PdfCraft Plus folders under
`~/Library/Application Support/` and `~/Library/Preferences/` that carry its name or bundle
identifier.

## Build from source

You need the Xcode Command Line Tools (`xcode-select --install`) and Rust 1.95 or later from
[rustup](https://rustup.rs).

Run the app straight from a checkout:

```sh
git clone https://github.com/AbdusamadAzamatov/pdfcraft-plus
cd pdfcraft-plus
cargo xtask models                           # optional: the OCR models Scan & OCR needs
cargo run --release -p pdfcraft -- some.pdf  # or: cargo run -p pdfcraft (debug build)
```

Build the same DMG and CLI zip as a release:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
packaging/macos/package.sh --arch universal  # or --arch aarch64 / --arch x86_64 for one architecture
```

The files land in `dist/release/`. `package.sh` downloads the OCR models itself (`cargo xtask
models`), signs ad hoc unless `MACOS_SIGN_IDENTITY` names a real certificate and notarises only
when `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` are also set. A build you make on your own
Mac opens there without the Gatekeeper warning, because it was never downloaded.

The GitHub Actions workflow `.github/workflows/fork-release.yml` builds releases the same way: a
pushed tag `v<version>` that matches `[workspace.package] version` in `Cargo.toml` produces a draft
release with the DMG, the CLI zip and `SHA256SUMS.txt`, which the maintainer reviews and publishes.
