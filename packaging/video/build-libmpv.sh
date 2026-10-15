#!/usr/bin/env bash
# Builds the video runtime a release embeds: one self-contained libmpv,
# LGPL only, for the platform it runs on (Linux, macOS, or Windows under
# MSYS2's UCRT64 shell). The application loads it at run time
# (crates/app/src/video.rs), so the application itself stays Apache-2.0.
#
#   packaging/video/build-libmpv.sh --out <runtime.tar.gz> [--work <dir>] [--jobs <n>]
#
# What is in it: libmpv with FFmpeg, libplacebo, libass, FreeType,
# FriBidi and HarfBuzz linked in statically, and nothing GPL or nonfree:
# FFmpeg is configured without --enable-gpl, --enable-version3 or
# --enable-nonfree, mpv with -Dgpl=false, and both are checked after the
# build. Every source is pinned by version and checksum. The archive
# carries the libraries' licences and SOURCES.txt, where each source was
# taken from: what the LGPL asks of whoever passes the library on.
#
# The library may depend on the system and nothing else: the C library,
# ALSA on Linux, Apple's frameworks on macOS, Windows' own DLLs. The
# build fails when it depends on anything more.
#
# What it needs: a C compiler, meson, ninja, pkg-config, nasm (x86 only),
# python3, curl, git, tar and xz. docs/RELEASING.md lists them per
# platform, as the release workflow installs them.

set -euo pipefail

out=""
work=""
jobs=""
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --work) work="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
if [ -z "$out" ]; then
    echo "usage: $0 --out <runtime.tar.gz> [--work <dir>] [--jobs <n>]" >&2
    exit 2
fi

here="$(cd "$(dirname "$0")" && pwd)"
case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    MINGW*|MSYS*) os=windows ;;
    *) echo "unsupported system: $(uname -s)" >&2; exit 2 ;;
esac
arch="$(uname -m)"
work="${work:-$PWD/libmpv-work}"
mkdir -p "$work"
work="$(cd "$work" && pwd)"
out_dir="$(dirname "$out")"
mkdir -p "$out_dir"
out="$(cd "$out_dir" && pwd)/$(basename "$out")"
if [ -z "$jobs" ]; then
    jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
fi

src="$work/src"
build="$work/build"
prefix="$work/prefix"
stage="$work/stage"
rm -rf "$build" "$prefix" "$stage"
mkdir -p "$src" "$build" "$prefix" "$stage/LICENSES"

# ----- the sources, pinned -------------------------------------------------

FFMPEG_URL=https://ffmpeg.org/releases/ffmpeg-7.1.1.tar.xz
FFMPEG_SHA256=733984395e0dbbe5c046abda2dc49a5544e7e0e1e2366bba849222ae9e3a03b1
MPV_URL=https://github.com/mpv-player/mpv/archive/refs/tags/v0.40.0.tar.gz
MPV_SHA256=10a0f4654f62140a6dd4d380dcf0bbdbdcf6e697556863dc499c296182f081a3
LIBASS_URL=https://github.com/libass/libass/releases/download/0.17.3/libass-0.17.3.tar.xz
LIBASS_SHA256=eae425da50f0015c21f7b3a9c7262a910f0218af469e22e2931462fed3c50959
FREETYPE_URL=https://downloads.sourceforge.net/project/freetype/freetype2/2.13.3/freetype-2.13.3.tar.xz
FREETYPE_SHA256=0550350666d427c74daeb85d5ac7bb353acba5f76956395995311a9c6f063289
FRIBIDI_URL=https://github.com/fribidi/fribidi/releases/download/v1.0.16/fribidi-1.0.16.tar.xz
FRIBIDI_SHA256=1b1cde5b235d40479e91be2f0e88a309e3214c8ab470ec8a2744d82a5a9ea05c
HARFBUZZ_URL=https://github.com/harfbuzz/harfbuzz/releases/download/10.4.0/harfbuzz-10.4.0.tar.xz
HARFBUZZ_SHA256=480b6d25014169300669aa1fc39fb356c142d5028324ea52b3a27648b9beaad8
# libplacebo's release archives leave its submodules out: a clone of the
# tag, held to the commit the tag names.
LIBPLACEBO_GIT=https://code.videolan.org/videolan/libplacebo.git
LIBPLACEBO_TAG=v7.351.0
LIBPLACEBO_COMMIT=3188549fba13bbdf3a5a98de2a38c2e71f04e21e

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Downloads `url` once, checks it, and unpacks it under build/.
fetch() {
    local url="$1" sum="$2" file
    file="$src/$(basename "$url")"
    if [ ! -f "$file" ] || [ "$(sha256 "$file")" != "$sum" ]; then
        curl -fsSL --retry 5 --retry-delay 3 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    local got
    got="$(sha256 "$file")"
    if [ "$got" != "$sum" ]; then
        echo "checksum mismatch for $url: $got" >&2
        exit 1
    fi
    tar -xf "$file" -C "$build"
    echo "$url sha256:$sum" >> "$stage/SOURCES.txt"
}

: > "$stage/SOURCES.txt"
fetch "$FFMPEG_URL" "$FFMPEG_SHA256"
fetch "$MPV_URL" "$MPV_SHA256"
fetch "$LIBASS_URL" "$LIBASS_SHA256"
fetch "$FREETYPE_URL" "$FREETYPE_SHA256"
fetch "$FRIBIDI_URL" "$FRIBIDI_SHA256"
fetch "$HARFBUZZ_URL" "$HARFBUZZ_SHA256"
git clone --quiet --depth 1 --branch "$LIBPLACEBO_TAG" --recurse-submodules \
    --shallow-submodules "$LIBPLACEBO_GIT" "$build/libplacebo"
got="$(git -C "$build/libplacebo" rev-parse HEAD)"
if [ "$got" != "$LIBPLACEBO_COMMIT" ]; then
    echo "libplacebo $LIBPLACEBO_TAG is $got, expected $LIBPLACEBO_COMMIT" >&2
    exit 1
fi
echo "$LIBPLACEBO_GIT $LIBPLACEBO_TAG commit:$LIBPLACEBO_COMMIT (with its submodules)" >> "$stage/SOURCES.txt"

# ----- the toolchain -------------------------------------------------------

export PKG_CONFIG_PATH="$prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
cflags="-O2 -fPIC"
ldflags=""
case "$os" in
    macos)
        # The application's own floor (packaging/macos/bundle.sh).
        export MACOSX_DEPLOYMENT_TARGET=11.0
        cflags="$cflags -mmacosx-version-min=11.0"
        ldflags="-mmacosx-version-min=11.0"
        ;;
    windows)
        cflags="-O2"
        # The DLL carries its compiler runtime: no libgcc, libstdc++ or
        # libwinpthread DLL next to it.
        ldflags="-static-libgcc -static"
        ;;
esac
export CFLAGS="$cflags" CXXFLAGS="$cflags" LDFLAGS="$ldflags"

# Every dependency: static, and with whatever it would find on its own
# turned off, so that nothing of the build machine creeps in.
static_meson() {
    local dir="$1"
    shift
    meson setup "$build/$dir/_build" "$build/$dir" \
        --prefix="$prefix" --libdir=lib --buildtype=release \
        -Ddefault_library=static -Dauto_features=disabled "$@"
    ninja -C "$build/$dir/_build" -j "$jobs" install
}

# FreeType 2.13.3 looks for bzip2 whatever its option says: held to it.
sed -i.orig "s/dependency('bzip2', required: false)/dependency('bzip2', required: get_option('bzip2'))/" \
    "$build/freetype-2.13.3/meson.build"
grep -q "dependency('bzip2', required: get_option('bzip2'))" "$build/freetype-2.13.3/meson.build"
# Its zlib choice is not a feature: off by name, like the rest.
static_meson freetype-2.13.3 -Dtests=disabled -Dzlib=disabled -Dbzip2=disabled \
    -Dpng=disabled -Dbrotli=disabled -Dharfbuzz=disabled
static_meson fribidi-1.0.16 -Ddocs=false -Dbin=false -Dtests=false
static_meson harfbuzz-10.4.0 -Dfreetype=enabled -Dwith_libstdcxx=false \
    -Dtests=disabled -Dutilities=disabled -Ddocs=disabled
static_meson libass-0.17.3 -Drequire-system-font-provider=false
static_meson libplacebo -Ddemos=false -Dtests=false

# ----- FFmpeg: LGPL 2.1, what a WhatsApp file needs ------------------------

ffmpeg_dir="$build/ffmpeg-7.1.1"
ffmpeg_args=(
    --prefix="$prefix"
    --enable-static --disable-shared --enable-pic
    --disable-programs --disable-doc --disable-network
    --disable-autodetect --disable-debug
    --disable-everything
    --enable-protocol=file
    --enable-demuxer=mov,matroska,ogg,mp3,aac,wav,flac,gif,image2
    --enable-parser=h264,hevc,aac,aac_latm,mpeg4video,mpegaudio,vp8,vp9,av1,opus,vorbis,flac,mjpeg
    --enable-decoder=h264,hevc,mpeg4,vp8,vp9,av1,aac,aac_latm,mp3,mp3float,opus,vorbis,flac,alac,pcm_s16le,pcm_s16be,pcm_f32le,mjpeg,gif,png
    --enable-bsfs
    --enable-filters
    --enable-swscale --enable-swresample --enable-avfilter
)
case "$os/$arch" in
    macos/arm64) ;;
    */x86_64)
        command -v nasm >/dev/null || { echo "nasm is needed on x86" >&2; exit 1; }
        ;;
esac
(
    cd "$ffmpeg_dir"
    ./configure "${ffmpeg_args[@]}" --extra-cflags="$cflags" --extra-ldflags="$ldflags"
    make -j "$jobs"
    make install
)
# Nothing of FFmpeg that is not LGPL 2.1.
for flag in CONFIG_GPL CONFIG_NONFREE CONFIG_VERSION3; do
    if ! grep -q "^#define $flag 0$" "$ffmpeg_dir/config.h"; then
        echo "FFmpeg was configured with $flag: not an LGPL 2.1 build" >&2
        exit 1
    fi
done

# ----- libmpv --------------------------------------------------------------

mpv_dir="$build/mpv-0.40.0"
mpv_args=(
    -Dlibmpv=true -Dcplayer=false -Dgpl=false
    -Dtests=false -Dfuzzers=false -Dbuild-date=false
    # Frames are drawn by libmpv's software renderer: no OpenGL.
    -Dgl=disabled
    # Scripting is not a feature either: off by name.
    -Dlua=disabled
    -Dmanpage-build=disabled -Dhtml-build=disabled -Dpdf-build=disabled
)
case "$os" in
    linux) mpv_args+=(-Dalsa=enabled -Diconv=enabled) ;;
    # macOS has its iconv as a shared library only, and the build looks
    # for static ones: subtitles' character sets are all it is for.
    macos) mpv_args+=(-Dcoreaudio=enabled -Diconv=disabled -Dswift-build=disabled) ;;
    windows) mpv_args+=(-Dwasapi=enabled -Dwin32-threads=enabled) ;;
esac
case "$os" in
    linux)
        # Its own symbols only: the static libraries' stay inside.
        export LDFLAGS="$ldflags -Wl,--exclude-libs,ALL"
        ;;
    windows)
        # libplacebo has a little C++ (std::from_chars), and mpv is linked
        # as C: the C++ library goes in, statically like the rest.
        export LDFLAGS="$ldflags -lstdc++"
        ;;
esac
meson setup "$build/mpv/_build" "$mpv_dir" \
    --prefix="$prefix" --libdir=lib --buildtype=release \
    -Ddefault_library=shared -Dprefer_static=true -Dauto_features=disabled \
    "${mpv_args[@]}"
ninja -C "$build/mpv/_build" -j "$jobs"
if ! grep -q "^#define HAVE_GPL 0$" "$build/mpv/_build/config.h"; then
    echo "mpv was configured with GPL parts: not an LGPL build" >&2
    exit 1
fi

# ----- the library, checked ------------------------------------------------

case "$os" in
    linux)
        library="$(find "$build/mpv/_build" -maxdepth 1 -name 'libmpv.so.*' -type f | head -n 1)"
        name="libmpv.so.2"
        cp "$library" "$stage/$name"
        strip --strip-unneeded "$stage/$name"
        needed="$(readelf -d "$stage/$name" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')"
        allowed='^(libc\.so\.6|libm\.so\.6|libpthread\.so\.0|libdl\.so\.2|librt\.so\.1|libasound\.so\.2|ld-linux-x86-64\.so\.2|ld-linux-aarch64\.so\.1)$'
        ;;
    macos)
        library="$(find "$build/mpv/_build" -maxdepth 1 -name 'libmpv.*.dylib' -type f | head -n 1)"
        name="libmpv.2.dylib"
        cp "$library" "$stage/$name"
        strip -x "$stage/$name"
        install_name_tool -id "@rpath/$name" "$stage/$name"
        # Apple Silicon runs nothing unsigned: an ad-hoc signature, which
        # the release replaces with its Developer ID.
        codesign --force --sign - "$stage/$name"
        needed="$(otool -L "$stage/$name" | tail -n +2 | awk '{print $1}' | grep -v "^@rpath/$name$" || true)"
        allowed='^(/usr/lib/|/System/Library/)'
        ;;
    windows)
        library="$(find "$build/mpv/_build" -maxdepth 1 -name 'libmpv-2.dll' -type f | head -n 1)"
        name="libmpv-2.dll"
        cp "$library" "$stage/$name"
        strip --strip-unneeded "$stage/$name"
        needed="$(objdump -p "$stage/$name" | sed -n 's/.*DLL Name: \(.*\)/\1/p')"
        # Windows' own DLLs and the universal C runtime.
        allowed='^((api-ms-win-|ext-ms-win-).*|(kernel32|user32|gdi32|advapi32|shell32|ole32|oleaut32|ws2_32|bcrypt|crypt32|secur32|shlwapi|winmm|avrt|ksuser|mfplat|mf|mfuuid|propsys|uuid|version|dwmapi|uxtheme|imm32|ntdll|ucrtbase|msvcrt|setupapi|cfgmgr32|dxgi|d3d11|dwrite|usp10|comdlg32|winspool|shcore)\.dll)$'
        ;;
esac
if [ -z "$library" ]; then
    echo "the build left no libmpv" >&2
    exit 1
fi
bad=""
while IFS= read -r dep; do
    [ -z "$dep" ] && continue
    if ! printf '%s\n' "$dep" | grep -Eiq "$allowed"; then
        bad="$bad $dep"
    fi
done <<< "$needed"
if [ -n "$bad" ]; then
    echo "libmpv depends on more than the system:$bad" >&2
    exit 1
fi
echo "libmpv depends on: $(printf '%s ' $needed)"

# ----- the licences, and the archive ---------------------------------------

cp "$ffmpeg_dir/COPYING.LGPLv2.1" "$stage/LICENSES/FFmpeg-LGPL-2.1.txt"
cp "$mpv_dir/LICENSE.LGPL" "$stage/LICENSES/mpv-LGPL-2.1.txt"
cp "$build/libplacebo/LICENSE" "$stage/LICENSES/libplacebo-LGPL-2.1.txt"
cp "$build/libass-0.17.3/COPYING" "$stage/LICENSES/libass-ISC.txt"
cp "$build/freetype-2.13.3/docs/FTL.TXT" "$stage/LICENSES/FreeType-FTL.txt"
cp "$build/fribidi-1.0.16/COPYING" "$stage/LICENSES/FriBidi-LGPL-2.1.txt"
cp "$build/harfbuzz-10.4.0/COPYING" "$stage/LICENSES/HarfBuzz-MIT.txt"
cat > "$stage/README.txt" <<EOF
The video runtime of Wuapi Inbox: libmpv ($name), built by
packaging/video/build-libmpv.sh for $os-$arch, LGPL 2.1 or later, with
FFmpeg, libplacebo, libass, FreeType, FriBidi and HarfBuzz linked in.
Their licences are in LICENSES/, and SOURCES.txt says where each source
was taken from. The application loads this library at run time; setting
WUAPI_LIBMPV_PATH to another build of libmpv replaces it.
EOF
# By name: no `./` entry.
tar -czf "$out" -C "$stage" "$name" LICENSES SOURCES.txt README.txt
echo "wrote $out ($(du -h "$out" | cut -f1))"
