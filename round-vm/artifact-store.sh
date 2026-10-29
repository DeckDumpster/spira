# THE ARTIFACT STORE (release/src/artifacts.rs is the same protocol): the tree's release
# executables are stored once, under tree + toolchain + profile, on the store the sccache above
# already uses. A hit needs the manifest, written last, to meet the tarball's and every member's
# checksum; anything else is a miss and the build runs as it always did.
art_get() { curl -sS --max-time 600 -o "$2" -w '%{http_code}' "$art_base/$1" 2>/dev/null || true; }
art_put() { curl -sS -f --max-time 600 -o /dev/null -T "$2" "$art_base/$1"; }
art_key() { printf '%s %s release-workspace\n' "$1" "$2" | sha256sum | cut -d' ' -f1; }
art_fetch() {
    local key="$1" dest="$2" w name sum listed
    w="$(mktemp -d)"
    if [ "$(art_get "$key/MANIFEST" "$w/MANIFEST")" != 200 ]; then
        echo "round-vm: artifacts: miss $key (not stored)" >&2; rm -rf "$w"; return 1
    fi
    if ! grep -qx "key=$key" "$w/MANIFEST" || [ "$(art_get "$key/bin.tar" "$w/bin.tar")" != 200 ] \
        || [ "$(stat -c %s "$w/bin.tar")" != "$(sed -n 's/^tarball_bytes=//p' "$w/MANIFEST")" ] \
        || [ "$(sha256sum "$w/bin.tar" | cut -d' ' -f1)" != "$(sed -n 's/^tarball_sha256=//p' "$w/MANIFEST")" ]; then
        echo "round-vm: artifacts: miss $key (no tarball meeting its manifest)" >&2; rm -rf "$w"; return 1
    fi
    mkdir "$w/bin"
    if ! tar -xf "$w/bin.tar" -C "$w/bin"; then
        echo "round-vm: artifacts: miss $key (tarball does not unpack)" >&2; rm -rf "$w"; return 1
    fi
    listed="$(sed -n 's/^file [0-9a-f]* //p' "$w/MANIFEST" | sort)"
    if [ -z "$listed" ] || [ "$listed" != "$(find "$w/bin" -maxdepth 1 -type f -executable -printf '%f\n' | sort)" ]; then
        echo "round-vm: artifacts: miss $key (members differ from the manifest)" >&2; rm -rf "$w"; return 1
    fi
    while read -r _ sum name; do
        if [ "$(sha256sum "$w/bin/$name" | cut -d' ' -f1)" != "$sum" ]; then
            echo "round-vm: artifacts: miss $key ($name does not match its checksum)" >&2; rm -rf "$w"; return 1
        fi
    done < <(grep '^file ' "$w/MANIFEST")
    mkdir -p "$dest"
    cp -p "$w"/bin/* "$dest"/
    rm -rf "$w"
    echo "round-vm: artifacts: hit $key" >&2
}
art_store() {
    local key="$1" tree="$2" tc="$3" dir="$4" w names n
    w="$(mktemp -d)"
    names="$(find "$dir" -maxdepth 1 -type f -executable -printf '%f\n' | sort)"
    [ -n "$names" ] || { rm -rf "$w"; return 1; }
    # shellcheck disable=SC2086
    tar -cf "$w/bin.tar" -C "$dir" $names || { rm -rf "$w"; return 1; }
    {
        printf 'key=%s\ntree=%s\ntoolchain=%s\ntarball_sha256=%s\ntarball_bytes=%s\n' "$key" "$tree" "$tc" "$(sha256sum "$w/bin.tar" | cut -d' ' -f1)" "$(stat -c %s "$w/bin.tar")"
        for n in $names; do printf 'file %s %s\n' "$(sha256sum "$dir/$n" | cut -d' ' -f1)" "$n"; done
    } > "$w/MANIFEST"
    if art_put "$key/bin.tar" "$w/bin.tar" && art_put "$key/MANIFEST" "$w/MANIFEST"; then
        echo "round-vm: artifacts: stored $key" >&2; rm -rf "$w"
    else
        echo "round-vm: artifacts: not stored $key (the store refused it)" >&2; rm -rf "$w"; return 1
    fi
}
