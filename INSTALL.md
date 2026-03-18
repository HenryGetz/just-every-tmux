# Installation Guide

This guide is intentionally explicit and copy/paste-friendly.

## 1) Linux (Ubuntu/Debian)

Install dependencies:

```bash
sudo apt update
sudo apt install -y tmux curl build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

Install `just-every-tmux`:

```bash
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
./install
```

Verify:

```bash
br --help
b --help
cx --help
```

## 2) Linux (Fedora)

```bash
sudo dnf install -y tmux curl gcc openssl-devel pkgconf-pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
./install
```

## 3) Linux (Arch)

```bash
sudo pacman -S --noconfirm tmux curl base-devel openssl pkgconf rustup
rustup default stable

git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
./install
```

## 4) macOS

Install dependencies with Homebrew:

```bash
brew install tmux rust
```

Install `just-every-tmux`:

```bash
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
./install
```

If `~/.local/bin` is not in PATH, add this to `~/.zshrc` or `~/.bashrc`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

Then restart shell and verify:

```bash
br --help
b --help
cx --help
```

## 5) Windows (Native PowerShell)

Fast path from PowerShell:

```powershell
git clone https://github.com/HenryGetz/just-every-tmux.git
cd just-every-tmux
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
```

This script:

- builds native Windows binaries (`br.exe`, `b.exe`, `cx.exe`)
- installs them to `~\.local\bin`
- auto-installs `psmux` (tmux-compatible command) if `tmux` is missing
- uses Windows user profile paths by default (including `%USERPROFILE%\.code`)

Verify in a new PowerShell window:

```powershell
br --help
b --help
cx --help
tmux --help
```

If you want to skip `psmux` auto-install:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -SkipPsmux
```

## Upgrading

```bash
cd just-every-tmux
git pull
./install
```

## Optional Interactive Installer (Linux/macOS)

If you want a guided installer menu:

```bash
./scripts/install-tui.sh
```

## Uninstall

```bash
rm -f "$HOME/.local/bin/br" "$HOME/.local/bin/b" "$HOME/.local/bin/cx"
```
