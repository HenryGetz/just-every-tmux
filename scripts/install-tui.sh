#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_SCRIPT="$ROOT_DIR/scripts/install.sh"

if [[ ! -x "$INSTALL_SCRIPT" ]]; then
  echo "error: $INSTALL_SCRIPT is not executable" >&2
  exit 1
fi

show_menu() {
  cat <<'EOF'

just-every-tmux installer
=========================
1) Recommended install (to ~/.local/bin)
2) Custom install prefix
3) Uninstall from ~/.local/bin
4) Quit

EOF
}

while true; do
  show_menu
  read -r -p "Choose [1-4]: " choice
  case "$choice" in
    1)
      "$INSTALL_SCRIPT"
      ;;
    2)
      read -r -p "Enter install prefix directory: " prefix
      if [[ -z "${prefix// }" ]]; then
        echo "No prefix entered; canceled."
      else
        PREFIX="$prefix" "$INSTALL_SCRIPT"
      fi
      ;;
    3)
      target="$HOME/.local/bin"
      rm -f "$target/br" "$target/b" "$target/cx"
      echo "Removed $target/br, $target/b, $target/cx"
      ;;
    4)
      echo "Bye."
      exit 0
      ;;
    *)
      echo "Invalid choice: $choice"
      ;;
  esac
done

