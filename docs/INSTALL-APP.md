# Installing the Omarchy Mobile installer on your computer

**Not published yet.** No download of the installer exists until the maintainer has run the checks in [GO-LIVE.md](GO-LIVE.md) and published the first release. Until then, this page says how it will work; the [command-line installer](https://github.com/derekross/omarchy-mobile#quick-start) is the way to install today.

The installer is a desktop app, "Omarchy Mobile Installer". It walks you through putting Omarchy Mobile on your phone, which is plugged into this computer with a USB cable. **Installing erases the phone.** The app says so, and writes nothing until you have typed your phone's codename.

It runs on Linux on a 64-bit PC (x86_64). Windows and macOS are not supported.

## Get the files

On the installer's [Releases page](https://github.com/derekross/omamobile-installer/releases), the newest release has:

| File | For |
| --- | --- |
| `omainstall-X.Y.Z-1-x86_64.pkg.tar.zst` | Omarchy and Arch Linux |
| `omainstall-vX.Y.Z-x86_64.AppImage` | Other Linux distributions (Ubuntu 22.04 or newer, Debian 12 or newer, Fedora, and others from 2022 on) |
| `omamobile-installer-web-vX.Y.Z.tar.gz` | The web page's files, for omamobile.org; you don't need it |
| `SHA256SUMS` | The checksum of each file |

Download the one for your computer and `SHA256SUMS` into the same folder. Then open a terminal in that folder and check the download:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

It must print `OK` after your file's name. If it prints `FAILED`, delete the file and download it again; if it fails again, don't use it, and tell us in an issue.

## Omarchy and Arch Linux: the package

```sh
sudo pacman -U ./omainstall-X.Y.Z-1-x86_64.pkg.tar.zst
```

Type your password when asked. pacman installs everything the app needs along with it. Then open **Omarchy Mobile Installer** from the app launcher (on Omarchy: Super + Space, then type "installer").

To remove it later: `sudo pacman -R omainstall`.

## Other Linux distributions: the AppImage

An AppImage is one file that holds the app and what it needs to draw its window, so there is nothing to install for the window itself.

1. Make it runnable and start it:

   ```sh
   chmod +x omainstall-vX.Y.Z-x86_64.AppImage
   ./omainstall-vX.Y.Z-x86_64.AppImage
   ```

   If it says something about FUSE, install your distribution's FUSE 2 library (Ubuntu 22.04: `sudo apt install libfuse2`; Ubuntu 24.04: `sudo apt install libfuse2t64`; Fedora: `sudo dnf install fuse-libs`), or start it with `./omainstall-vX.Y.Z-x86_64.AppImage --appimage-extract-and-run`.

2. Install the tools the install uses after it has written the phone. The app runs Omarchy Mobile's own setup scripts, which unpack the firmware from Google's factory image and talk to the phone over SSH. They need these commands: `bash`, `python3`, `ssh` and `ssh-keygen`, `unzip`, `lpunpack` and `simg2img`, `fsck.erofs`, `debugfs`, `readelf`, `lz4`, `zstd` and `cpio`.

   - Fedora: `sudo dnf install python3 openssh-clients unzip android-tools erofs-utils e2fsprogs binutils lz4 zstd cpio`
   - Ubuntu and Debian: `sudo apt install python3 openssh-client unzip android-sdk-libsparse-utils erofs-utils e2fsprogs binutils lz4 zstd cpio`. Ubuntu and Debian don't package `lpunpack` everywhere; check with `command -v lpunpack`. Without it, the firmware step can't run, and Omarchy or Arch is the way to finish an install for now.

   NetworkManager is optional. With it, the app adds the USB connection to the phone when you click; without it, give this computer the address 172.16.42.2/24 on the phone's USB network connection yourself.

3. Let the app reach the phone over USB. A current distribution (systemd 262 or newer) already lets you open a phone in Fastboot Mode. On an older one, copy [platform/linux/70-omamobile-fastboot.rules](../platform/linux/70-omamobile-fastboot.rules) into `/etc/udev/rules.d/`, then run `sudo udevadm control --reload` and unplug and replug the phone.

To remove it later, delete the AppImage file.

## What the app leaves on your computer

Keep the radio backup: it is the only copy of your phone's own calibration and radio data. The rest you can delete once the phone works.

- `~/omamobile-backup/`: the radio backup. **Keep it.**
- `~/.local/state/omamobile-installer/`: how far the install got, so a closed window can pick up where it stopped.
- `~/.local/share/omamobile-installer/ssh/`: a key the app made, if you had none.
- `~/.cache/omamobile-installer/`: Google's factory image, downloaded.
- A NetworkManager connection named `omamobile-phone`, if you let the app add it (`nmcli connection delete omamobile-phone` removes it).

## From a browser instead

A web page version runs at omamobile.org once it is switched on, in Chrome, Edge and other Chromium-based browsers. For now it checks the phone and the release and shows what an install would write, but it does not write to the phone.
