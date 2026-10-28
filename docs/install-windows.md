# Installing PdfCraft Plus on Windows

PdfCraft Plus is a personal fork of [PdfCraft](https://github.com/storytold/pdfcraft). This page
explains how to install the ready-made Windows build from
[GitHub Releases](https://github.com/AbdusamadAzamatov/pdfcraft-plus/releases), what to do about
the SmartScreen warning on first launch, and how to build it yourself.

## Requirements

- Windows 10 or Windows 11, 64-bit, on an x64 (Intel or AMD) processor. Settings › System ›
  About shows it as *System type: 64-bit operating system, x64-based processor*.
- Windows on ARM (Snapdragon and similar) and 32-bit Windows have no ready-made build yet; see
  [Build from source](#build-from-source).

## What each release contains

| File | What it is |
|---|---|
| `pdfcraft-plus-<version>-windows-x64.msi` | The installer: PdfCraft Plus for all users, with Start menu and desktop shortcuts |
| `pdfcraft-plus-<version>-windows-x64-portable.zip` | The portable version: unzip anywhere and run it, nothing is installed |
| `SHA256SUMS.txt` | A SHA-256 checksum for every file of the release |

Both contain the app, `pdfcraft-plus.exe`, the command-line tool, `pdfcraft-cli.exe`, and the OCR
models. `<version>` is the release number, for example `0.14.3`.

## Install with the MSI

1. Open the [latest release](https://github.com/AbdusamadAzamatov/pdfcraft-plus/releases/latest)
   and download `pdfcraft-plus-<version>-windows-x64.msi`.
2. Double-click it. If SmartScreen warns you, see [below](#first-launch-windows-protected-your-pc).
3. The first page has two choices, both on by default: a desktop shortcut, and **Edit with
   PdfCraft Plus** on the right-click menu of PDF files. Pictures also get **Create PDF with
   PdfCraft Plus…** on their right-click menu. Click **Install** and allow the change when Windows
   asks (the installer puts PdfCraft Plus in `C:\Program Files\PdfCraft Plus` for every user of
   the PC).
4. Open PdfCraft Plus from the Start menu or the desktop.

PdfCraft Plus never makes itself the default PDF app. To use it for PDF files, right-click a PDF,
choose **Open with › Choose another app**, pick **PdfCraft Plus** and tick **Always**, or set it in
**Settings › Apps › Default apps**. On Windows 11 the **Edit with PdfCraft Plus** entry is under
**Show more options**.

For an unattended install, run in an administrator terminal:

```powershell
msiexec /i pdfcraft-plus-<version>-windows-x64.msi /qn ALLUSERS=1
```

Add `INSTALLDESKTOPSHORTCUT=0`, `INSTALLPDFCONTEXTMENU=0` or `INSTALLIMAGECONTEXTMENU=0` to leave
the desktop shortcut or a right-click menu out.

## Use the portable version

1. Download `pdfcraft-plus-<version>-windows-x64-portable.zip`.
2. Right-click it, choose **Extract All…**, and pick a folder you can write to (your Documents
   folder or a USB stick, not `C:\Program Files`).
3. Open the extracted folder and run `pdfcraft-plus.exe`.

The `portable.txt` file beside it keeps PdfCraft Plus portable: settings, logs and crash-recovery
files go to a `PdfCraftData` folder next to the program and nothing is written elsewhere, so the
whole folder can be moved or copied. Delete `portable.txt` to use the normal per-user folders
instead.

## First launch: "Windows protected your PC"

The Windows build is **not signed with a code-signing certificate** yet, so Microsoft Defender
SmartScreen doesn't know its publisher and may stop the first launch (of the MSI, or of
`pdfcraft-plus.exe` from the portable zip) with *"Windows protected your PC. Microsoft Defender
SmartScreen prevented an unrecognized app from starting."* Nothing is wrong with the download;
SmartScreen just has no reputation for it.

To allow it:

1. Click **More info**.
2. Click **Run anyway**.

Windows may also show *"Do you want to allow this app from an unknown publisher to make changes
to your device?"* when the MSI installs; click **Yes**. A downloaded zip can mark the files inside
as coming from the internet: if the portable version is blocked, right-click the zip before
extracting it, choose **Properties**, tick **Unblock** and click **OK**.

## Verify the download

Save `SHA256SUMS.txt` from the same release next to the file, then in PowerShell:

```powershell
cd $HOME\Downloads
Get-FileHash .\pdfcraft-plus-<version>-windows-x64.msi -Algorithm SHA256
Select-String 'windows-x64.msi' .\SHA256SUMS.txt
```

The two hashes must be the same (`Get-FileHash` prints it in capitals, the file in lower case).

## The command-line tool

`pdfcraft-cli.exe` sits beside the app (`C:\Program Files\PdfCraft Plus` after the MSI). It drives
the same engine headlessly, for scripts and AI agents:

```powershell
& "C:\Program Files\PdfCraft Plus\pdfcraft-cli.exe" --version
& "C:\Program Files\PdfCraft Plus\pdfcraft-cli.exe" tools
```

## Settings and upstream PdfCraft

PdfCraft Plus keeps its settings in `%APPDATA%\PdfCraft Plus` and its crash-recovery files in
`%LOCALAPPDATA%\PdfCraft Plus`. Its installer has its own product code, program name
(`pdfcraft-plus.exe`), folder and registry entries, so it installs **beside** upstream PdfCraft,
never over it: both can be installed, run and uninstalled independently, and opening a file in one
never hands it to the other.

## Update

Download the newer MSI and run it: it replaces the installed version and keeps your choices
(a desktop shortcut you deleted stays deleted). For the portable version, extract the new zip and
copy your `PdfCraftData` folder into it.

## Uninstall

Open **Settings › Apps › Installed apps**, find **PdfCraft Plus**, and choose **Uninstall**. Your
settings stay in `%APPDATA%\PdfCraft Plus`; delete that folder to remove them too. The portable
version is removed by deleting its folder.

## Build from source

On Windows with [Rust](https://rustup.rs) (MSVC toolchain), the Windows SDK and WiX v5
(`dotnet tool install --global wix --version 5.0.2`):

```powershell
git clone https://github.com/AbdusamadAzamatov/pdfcraft-plus
cd pdfcraft-plus
cargo run --release -p pdfcraft                     # run the app
pwsh packaging/windows/package.ps1 -Arch x64        # or build the MSI and portable zip
```

`package.ps1` also takes `-Arch arm64` (cross-compiled, needs the MSVC ARM64 build tools) and
`-Arch x86`. The packages land in `dist\release`.
