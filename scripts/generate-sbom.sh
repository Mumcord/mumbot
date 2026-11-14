#!/usr/bin/env bash
# Writes one CycloneDX 1.5 SBOM per release variant and platform whose contents differ:
#   <variant>-<version>-<platform>.cdx.json
# with the variants mumbot (default features) and mumbot-tester (feature `tester`) and the
# platforms linux (both static musl archives and the binary in the Docker images),
# windows-x86_64 and macos-universal. Each lists the Rust crates compiled into that binary
# (cargo-cyclonedx, without build and dev dependencies) and the C libraries that crates build from
# bundled sources and link statically: libopus (opusic-sys) and SQLite (libsqlite3-sys).
# The release workflow runs it; it also works locally. Usage:
#   scripts/generate-sbom.sh <version> <out-dir>
# Needs cargo-cyclonedx at the version in scripts/sbom-tools.env and jq. Nothing is compiled.
set -euo pipefail

version="${1:?usage: generate-sbom.sh <version> <out-dir>}"
mkdir -p "${2:?out-dir missing}"
out_dir="$(cd "$2" && pwd)"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
# shellcheck source=scripts/sbom-tools.env
source scripts/sbom-tools.env

command -v jq >/dev/null 2>&1 || { echo "jq not found" >&2; exit 1; }
command -v cargo-cyclonedx >/dev/null 2>&1 || {
  echo "cargo-cyclonedx not found. Install: cargo install cargo-cyclonedx --version $CARGO_CYCLONEDX_VERSION --locked" >&2
  exit 1
}
installed="$(cargo-cyclonedx cyclonedx --version | awk '{ print $NF }')"
[ "$installed" = "$CARGO_CYCLONEDX_VERSION" ] \
  || echo "warning: cargo-cyclonedx $installed, but scripts/sbom-tools.env pins $CARGO_CYCLONEDX_VERSION" >&2

# variant, Cargo features ("-" for none); the same pairs as the release.yml matrix.
variants="
mumbot         -
mumbot-tester  tester
"
# platform, Rust targets (comma-separated). The targets of one platform must resolve to the same
# crates, which is checked below.
platforms="
linux            x86_64-unknown-linux-musl,aarch64-unknown-linux-musl
windows-x86_64   x86_64-pc-windows-msvc
macos-universal  aarch64-apple-darwin,x86_64-apple-darwin
"

tmp="$(mktemp -d)"
name="sbom-$$"
# cargo-cyclonedx writes its file next to Cargo.toml.
trap 'rm -rf "$tmp" "$root/$name.json"' EXIT

metadata="$tmp/metadata.json"
cargo metadata --locked --format-version 1 > "$metadata"

# Prints the source directory of the one crate with this name, after checking that it is built
# with its `bundled` feature, so the C library really comes from the crate and not from the system.
bundled_crate_dir() {
  local crate="$1" id manifest
  id="$(jq -r --arg c "$crate" '.packages[] | select(.name == $c) | .id' "$metadata")"
  [ -n "$id" ] && [ "$(wc -l <<<"$id")" -eq 1 ] \
    || { echo "expected exactly one $crate in the dependency graph" >&2; return 1; }
  jq -e --arg id "$id" '.resolve.nodes[] | select(.id == $id) | .features | index("bundled")' \
    "$metadata" >/dev/null \
    || { echo "$crate is not built with its bundled feature; update scripts/generate-sbom.sh" >&2; return 1; }
  manifest="$(jq -r --arg id "$id" '.packages[] | select(.id == $id) | .manifest_path' "$metadata")"
  dirname "$manifest"
}

opus_dir="$(bundled_crate_dir opusic-sys)"
opus="$(sed -n 's/^PACKAGE_VERSION="\(.*\)"/\1/p' "$opus_dir/opus/package_version")"
[ -n "$opus" ] || { echo "libopus version not found in opusic-sys" >&2; exit 1; }
sqlite_dir="$(bundled_crate_dir libsqlite3-sys)"
sqlite="$(sed -n 's/^#define SQLITE_VERSION  *"\(.*\)"/\1/p' "$sqlite_dir/sqlite3/sqlite3.h")"
[ -n "$sqlite" ] || { echo "SQLite version not found in libsqlite3-sys" >&2; exit 1; }

# Writes the crates of one target and feature set to the given file.
rust_bom() {
  local target="$1" features="$2" out="$3"
  local args=()
  [ "$features" = "-" ] || args=(--features "$features")
  cargo-cyclonedx cyclonedx --format json --target "$target" "${args[@]}" --no-build-deps \
    --spec-version 1.5 --override-filename "$name" --quiet
  mv "$name.json" "$out"
}

while read -r variant features; do
  [ -n "$variant" ] || continue
  while read -r platform targets; do
    [ -n "$platform" ] || continue
    first=""
    for target in ${targets//,/ }; do
      bom="$tmp/$variant-$target.json"
      rust_bom "$target" "$features" "$bom"
      if [ -z "$first" ]; then
        first="$bom"
      elif ! diff -q <(jq -S '.components' "$first") <(jq -S '.components' "$bom") >/dev/null; then
        echo "$platform: $target has other crates than ${targets%%,*}; give it its own platform in scripts/generate-sbom.sh" >&2
        exit 1
      fi
    done
    out="$out_dir/$variant-$version-$platform.cdx.json"
    # The root gets the variant's name and the release version, and loses the build machine's
    # path. The bundled C libraries become components that their crates depend on.
    jq --arg variant "$variant" --arg version "$version" --arg platform "$platform" \
      --arg targets "$targets" --arg features "$features" --arg opus "$opus" --arg sqlite "$sqlite" '
      def native($name; $version; $licence; $cpe; $description; $url):
        { type: "library", "bom-ref": "native:\($name)@\($version)", name: $name, version: $version,
          description: $description, scope: "required", licenses: [{ license: { id: $licence } }],
          cpe: $cpe, purl: "pkg:generic/\($name)@\($version)",
          externalReferences: [{ type: "website", url: $url }] };
      def crate_ref($crate): first(.components[] | select(.name == $crate) | ."bom-ref");
      .metadata.component."bom-ref" as $old
      | "\($variant)@\($version)" as $ref
      | crate_ref("opusic-sys") as $opus_crate
      | crate_ref("libsqlite3-sys") as $sqlite_crate
      | native("opus"; $opus; "BSD-3-Clause"; "cpe:2.3:a:opus-codec:opus:\($opus):*:*:*:*:*:*:*";
          "Opus audio codec, built from the sources bundled in the opusic-sys crate and linked statically";
          "https://opus-codec.org") as $opus_lib
      | native("sqlite"; $sqlite; "blessing"; "cpe:2.3:a:sqlite:sqlite:\($sqlite):*:*:*:*:*:*:*";
          "SQLite, built from the amalgamation bundled in the libsqlite3-sys crate and linked statically";
          "https://sqlite.org") as $sqlite_lib
      | .metadata.component |= (del(.components) | .name = $variant | .version = $version
          | ."bom-ref" = $ref | .purl = "pkg:github/mumcord/mumbot@v\($version)")
      | .metadata.properties = [
          { name: "mumbot:variant", value: $variant },
          { name: "mumbot:platform", value: $platform },
          { name: "mumbot:targets", value: $targets },
          { name: "mumbot:features", value: (if $features == "-" then "" else $features end) }]
      | .components += [$opus_lib, $sqlite_lib]
      | .dependencies |= (map(
            if .ref == $old then .ref = $ref
            elif .ref == $opus_crate then .dependsOn += [$opus_lib."bom-ref"]
            elif .ref == $sqlite_crate then .dependsOn += [$sqlite_lib."bom-ref"]
            else . end)
          + [{ ref: $opus_lib."bom-ref" }, { ref: $sqlite_lib."bom-ref" }])
      ' "$first" > "$out"
    echo "Wrote $out"
  done <<<"$platforms"
done <<<"$variants"
