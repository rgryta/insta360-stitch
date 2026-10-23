#!/usr/bin/env bash
# Makes the npm packages from the Node.js modules that the package workflow built. It does not publish them.
# Usage (from the repository root): scripts/pack-npm.sh <artifacts folder> <output folder>
# The artifacts folder contains the node-* folders. The script changes bindings/node, thus use it in CI only.
set -euo pipefail

# Node gives the full paths, also on Windows, where the paths of Git Bash (/c/...) do not work for Node.
artifacts=$(node -p 'require("path").resolve(process.argv[1])' "$1")
mkdir -p "$2"
out=$(node -p 'require("path").resolve(process.argv[1])' "$2")
cd bindings/node

# Each npm/<platform> folder is one package with one .node file. napi copies each .node file into its folder.
# napi adds the folder to its working directory, thus it must be a relative path.
npx napi artifacts -d "$(node -p 'require("path").relative(process.cwd(), process.argv[1])' "$artifacts")"
version=$(node -p "require('./package.json').version")
for dir in npm/*/; do
  [ -n "$(find "$dir" -name '*.node')" ] || { echo "no Node.js module for $dir"; exit 1; }
  name=$(node -p "require('./${dir}package.json').name")
  # The main package finds its platform package through optionalDependencies. npm installs only the one that agrees
  # with the os and cpu of the computer. The packages are not in the registry yet, thus package.json does not list them.
  npm pkg set "optionalDependencies.$name=$version"
  (cd "$dir" && npm pack --pack-destination "$out")
done
npm pack --pack-destination "$out"
