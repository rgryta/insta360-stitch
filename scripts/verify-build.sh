#!/usr/bin/env bash
# Checks the package builds on the platform where they run. It does not build anything.
# Usage (from the repository root): scripts/verify-build.sh <rust target> <artifacts folder>
# The artifacts folder contains the folders that the package workflow uploads: cli-*, npm-packages, wheel-*.
set -euo pipefail

target=$1
artifacts=$2
fixture=tests/fixtures/synthetic_x5.insp
# The folder is in the repository: the Linux checks run the CLI in docker, which can see only the repository.
out=target/verify-build
rm -rf "$out" && mkdir -p "$out"

step() { echo "::group::$*"; }
done_step() { echo "::endgroup::"; }

# Checks a CLI: the version, the trailer data, a photo stitch and the preview.
# The arguments are the command that starts the CLI, for example "docker run ... /w/cli".
check_cli() {
  "$@" --version
  "$@" info "$fixture" | tee "$out/info.json"
  grep -q '"stitchable": true' "$out/info.json"
  "$@" photo "$fixture" "$out/photo.jpg"
  "$@" preview "$fixture" "$out/preview.jpg"
  for jpeg in "$out/photo.jpg" "$out/preview.jpg"; do
    # A JPEG starts with the SOI marker FF D8.
    [ "$(head -c 2 "$jpeg" | od -An -tx1 | tr -d ' ')" = "ffd8" ] || { echo "$jpeg is not a JPEG"; exit 1; }
  done
}

# Installs the npm packages into a new project, as a user does, and runs the Node.js tests against that install.
# The arguments are the command that runs the shell command, for example "docker run ... node:20-bullseye-slim" or "env".
check_npm() {
  mkdir -p "$out/npm-app"
  cp "$npm_main" "$npm_platform" "$out/npm-app/"
  "$@" sh -c "
    set -e
    cd '$out/npm-app' && npm init -y >/dev/null && npm install --no-audit --no-fund ./*.tgz
    cd ../../../bindings/node && node -p process.arch
    INSTA360_STITCH_MODULE=\"\$(node -p \"require('path').resolve('../../$out/npm-app/node_modules/insta360-stitch')\")\" node --test test/"
}

# Installs one wheel into a new virtual environment and runs the Python tests against it.
# The arguments are the wheel file and the uv Python request.
check_wheel() {
  local wheel=$1 python=$2 venv="$out/venv-$RANDOM"
  [ -f "$wheel" ] || { echo "no wheel for $python"; exit 1; }
  uv venv -q --python "$python" "$venv"
  uv pip install -q --python "$venv" "$wheel" pytest
  local bin="$venv/bin/python"
  [ -x "$bin" ] || bin="$venv/Scripts/python.exe"
  "$bin" -c "import sys, insta360_stitch; print(sys.version, 'GIL on' if getattr(sys, '_is_gil_enabled', lambda: True)() else 'GIL off')"
  "$bin" -m pytest -q -p no:cacheprovider bindings/python/tests
}

wheel() { find "$artifacts" -name '*.whl' | grep -E "$1" | head -n 1; }

cli=$(find "$artifacts/cli-$target" -type f -name 'insta360-stitch*' | head -n 1)
chmod +x "$cli"
# The npm platform package of the target, with the names of napi-rs.
case $target in
  x86_64-unknown-linux-gnu) platform=linux-x64-gnu ;;
  aarch64-unknown-linux-gnu) platform=linux-arm64-gnu ;;
  x86_64-apple-darwin) platform=darwin-x64 ;;
  aarch64-apple-darwin) platform=darwin-arm64 ;;
  x86_64-pc-windows-msvc) platform=win32-x64-msvc ;;
  aarch64-pc-windows-msvc) platform=win32-arm64-msvc ;;
  *) platform=unknown ;;
esac
npm_main=$(find "$artifacts/npm-packages" -name 'insta360-stitch-[0-9]*.tgz' | head -n 1)
npm_platform=$(find "$artifacts/npm-packages" -name "insta360-stitch-$platform-[0-9]*.tgz" | head -n 1)
[ -f "$npm_main" ] && [ -f "$npm_platform" ] || { echo "no npm packages for $platform"; exit 1; }

case $target in
  *-linux-gnu)
    arch=${target%%-*}
    # Debian bullseye has glibc 2.31. The builds use glibc 2.17, thus they must run there.
    step "CLI on Debian bullseye ($target)"
    check_cli docker run --rm -v "$PWD:/w" -w /w debian:bullseye-slim "/w/$cli"
    done_step
    step "npm packages on Debian bullseye ($target)"
    check_npm docker run --rm -v "$PWD:/w" -w /w node:20-bullseye-slim
    done_step
    step "abi3 wheel on Debian bullseye, Python 3.11 ($target)"
    w=$(wheel "cp311-abi3-manylinux.*_$arch\.whl")
    docker run --rm -v "$PWD:/w" -w /w python:3.11-slim-bullseye sh -c \
      "pip install -q '/w/$w' pytest && python -m pytest -q -p no:cacheprovider bindings/python/tests"
    done_step
    step "free-threaded wheel, Python 3.14t ($target)"
    check_wheel "$(wheel "cp314t-manylinux.*_$arch\.whl")" 3.14t
    done_step
    step "musllinux wheels on Alpine ($target)"
    w=$(wheel "cp311-abi3-musllinux.*_$arch\.whl")
    wt=$(wheel "cp314t-musllinux.*_$arch\.whl")
    docker run --rm -v "$PWD:/w" -w /w ghcr.io/astral-sh/uv:0.10.9-alpine sh -c "
      set -e
      for spec in '$w 3.12' '$wt 3.14t'; do
        set -- \$spec
        rm -rf /tmp/venv && uv venv -q --python \$2 /tmp/venv
        uv pip install -q --python /tmp/venv \"/w/\$1\" pytest
        /tmp/venv/bin/python -m pytest -q -p no:cacheprovider bindings/python/tests
      done"
    done_step
    ;;
  *-apple-darwin)
    if [ "$target" = x86_64-apple-darwin ]; then
      # The macOS runner is arm64. Rosetta 2 runs the x86_64 builds.
      prefix=(arch -x86_64)
      py_arch=macos-x86_64
    else
      prefix=()
      py_arch=macos-aarch64
    fi
    step "CLI ($target)"
    # bash 3.2 (macOS) refuses "${prefix[@]}" of an empty array with set -u.
    check_cli ${prefix[@]+"${prefix[@]}"} "$cli"
    done_step
    step "npm packages ($target)"
    check_npm env
    done_step
    step "universal2 wheels ($target)"
    check_wheel "$(wheel 'cp311-abi3-macosx.*universal2\.whl')" "cpython-3.12-$py_arch-none"
    check_wheel "$(wheel 'cp314t-macosx.*universal2\.whl')" "cpython-3.14t-$py_arch-none"
    done_step
    ;;
  *-windows-msvc)
    tag=$([ "${target%%-*}" = aarch64 ] && echo win_arm64 || echo win_amd64)
    step "CLI ($target)"
    check_cli "$cli"
    done_step
    step "npm packages ($target)"
    check_npm env
    done_step
    step "Windows wheels ($target)"
    check_wheel "$(wheel "cp311-abi3-$tag\.whl")" 3.12
    check_wheel "$(wheel "cp314t-$tag\.whl")" 3.14t
    done_step
    ;;
  *)
    echo "no checks for $target"
    exit 1
    ;;
esac
echo "all checks passed for $target"
