#!/usr/bin/env bash
# Builds depository and installs it for the current user as the desktop application "Repo Depot":
# the binary, its icon and a launcher entry. Set PREFIX to install somewhere other than ~/.local.
#
#   scripts/install.sh              build and install
#   scripts/install.sh --uninstall  remove what an install put there
set -euo pipefail

app_id=depository # the window's app id, so the desktop matches open windows to this entry
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
prefix=${PREFIX:-$HOME/.local}
bin=$prefix/bin/$app_id
icons=$prefix/share/icons/hicolor
icon=$icons/scalable/apps/$app_id.svg
apps=$prefix/share/applications
entry=$apps/$app_id.desktop

# Let the menus and the icon theme see the change without logging out.
refresh() {
	if command -v update-desktop-database >/dev/null; then
		update-desktop-database -q "$apps" || true
	fi
	# Only refresh a cache that's already there: a new one would go stale for other installs.
	if [[ -f $icons/icon-theme.cache ]] && command -v gtk-update-icon-cache >/dev/null; then
		gtk-update-icon-cache -q -f -t "$icons" || true
	fi
	for sycoca in kbuildsycoca6 kbuildsycoca5; do
		if command -v "$sycoca" >/dev/null; then
			"$sycoca" >/dev/null 2>&1 || true
			break
		fi
	done
}

case ${1:-} in
'') ;;
--uninstall)
	rm -fv -- "$bin" "$icon" "$entry"
	refresh
	echo "Uninstalled Repo Depot. Its settings and database are left in place."
	exit 0
	;;
*)
	echo "usage: $0 [--uninstall]" >&2
	exit 2
	;;
esac

cargo build --release --locked --manifest-path "$repo/Cargo.toml"

install -Dm755 "$repo/target/release/$app_id" "$bin"
install -Dm644 "$repo/assets/icon.svg" "$icon"
mkdir -p -- "$apps"
cat >"$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Repo Depot
GenericName=Repository Manager
Comment=Find, clone, tag and open your GitHub repositories
Exec="$bin"
Icon=$app_id
Terminal=false
Categories=Development;RevisionControl;
Keywords=git;github;repository;clone;
StartupNotify=true
StartupWMClass=$app_id
EOF

refresh
echo "Installed Repo Depot: $bin"
