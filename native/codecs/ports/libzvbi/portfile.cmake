# Looker's port of libzvbi for FFmpeg's --enable-libzvbi (DVB Teletext subtitles: the libzvbi_teletext decoder).
# configure finds it by pkg-config `zvbi-0.2`, header libzvbi.h, symbol vbi_decoder_new, and for a non-GPL FFmpeg
# wants >= 0.2.28. Upstream builds with autotools only, so the port brings its own CMakeLists.txt for MSVC that
# compiles just the LGPL Teletext decoder, with small Windows stand-ins in win32/ (pthread mutexes on SRW locks,
# an iconv for UCS-2 <-> UTF-8 and friends, gettimeofday). Result: zvbi-0.dll, no other DLLs.
vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO zapping-vbi/zvbi
    REF "v${VERSION}"
    SHA512 61d87331690730ebdd494d27cc7315b0386e97d4dc92cef4e90286b0eccca2ee452632baf37221ef47835d1ca56b883abd7315fb5c7cdf78fb9f04bb03b5e3ff
    HEAD_REF main
    PATCHES
        msvc.patch  # the GNU C that CMakeLists.txt can't rewrite mechanically: ?: with the middle left out,
                    # __attribute__((constructor)) (a .CRT$XCU entry instead), "\e", a named variadic macro
)

# Drop the GPL files so nothing can include or compile them (pdc.h and packet-830.h are regenerated from their
# public parts in libzvbi.h by CMakeLists.txt; the two decoders packet.c calls are stubbed in win32/looker_830.c).
file(REMOVE
    "${SOURCE_PATH}/src/packet-830.c"
    "${SOURCE_PATH}/src/packet-830.h"
    "${SOURCE_PATH}/src/pdc.c"
    "${SOURCE_PATH}/src/pdc.h"
    "${SOURCE_PATH}/src/exp-vtx.c"
)
file(COPY "${CMAKE_CURRENT_LIST_DIR}/CMakeLists.txt" DESTINATION "${SOURCE_PATH}")
file(COPY "${CMAKE_CURRENT_LIST_DIR}/win32" DESTINATION "${SOURCE_PATH}")

vcpkg_cmake_configure(SOURCE_PATH "${SOURCE_PATH}")
vcpkg_cmake_install()
vcpkg_copy_pdbs()
vcpkg_fixup_pkgconfig()

# COPYING.md: the per-file licences (src/* LGPL-2.0+, ure.* MIT, the GPL files this port leaves out).
vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/COPYING.md")
