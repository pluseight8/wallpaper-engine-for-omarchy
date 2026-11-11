# Maintainer: pluseight
pkgname=omawe-git
pkgver=0.1.0.r5.g8e2aaec
pkgrel=1
pkgdesc="Steam Wallpaper Engine wallpapers for Omarchy: library, per-monitor apply, auto-pause, theme sync"
arch=('x86_64')
url="https://github.com/pluseight8/wallpaper-engine-for-omarchy"
license=('MIT')
depends=('linux-wallpaperengine' 'imagemagick' 'jq' 'curl')
makedepends=('cargo' 'git')
optdepends=('steam: the Wallpaper Engine library itself')
provides=('omawe')
conflicts=('omawe')
install=omawe.install
source=("omawe::git+$url.git")
sha256sums=('SKIP')

pkgver() {
  cd omawe
  printf '0.1.0.r%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd omawe
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd omawe
  cargo build --frozen --release
}

check() {
  cd omawe
  # Unit tests only: the integration test needs a writable XDG_RUNTIME_DIR.
  cargo test --frozen --release -p omawed --bin omawed
}

package() {
  cd omawe
  install -Dm755 target/release/omawed target/release/omawe target/release/omawe-tui -t "$pkgdir/usr/bin"
  install -Dm755 integration/bin/* -t "$pkgdir/usr/bin"
  install -Dm644 integration/systemd/omawed.service "$pkgdir/usr/lib/systemd/user/omawed.service"
  sed -i 's|%h/.local/bin/omawed|/usr/bin/omawed|' "$pkgdir/usr/lib/systemd/user/omawed.service"
  install -d "$pkgdir/usr/share/omawe/plugin" "$pkgdir/usr/share/omawe/integration"
  cp -r manifest.json BarWidget.qml Panel.qml i18n LICENSE "$pkgdir/usr/share/omawe/plugin/"
  cp -r integration/hypr integration/hooks "$pkgdir/usr/share/omawe/integration/"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
}
