# Looker's port of liblc3 (Apache-2.0) for FFmpeg's --enable-liblc3 (configure wants pkg-config `lc3 >= 1.1.0`).
# Upstream's own meson build, library only (no tools, no Python), as lc3-1.dll.
vcpkg_from_github(
    OUT_SOURCE_PATH SOURCE_PATH
    REPO google/liblc3
    REF "v${VERSION}"
    SHA512 0f06367651b2ebd5e5e8f8e6256fd818646f6674e5bd278ba224c2fdbce7e4b6985d8ef1c5e69aee2fdbeeb4f790fb1e80529d3f8da9fc2dff98e9b8f90d4bdb
    HEAD_REF main
)

# lc3_private.h marks every API function __declspec(dllexport) on Windows, for users of the header too. Export
# only while building the DLL and import everywhere else (or plain declarations for a static library).
if(VCPKG_LIBRARY_LINKAGE STREQUAL "dynamic")
    set(lc3_user_decl "__declspec(dllimport)")
else()
    set(lc3_user_decl "")
endif()
vcpkg_replace_string("${SOURCE_PATH}/include/lc3_private.h"
    "#ifdef _WIN32\n#define LC3_EXPORT __declspec(dllexport)\n"
    "#if defined(_WIN32) && defined(LC3_BUILDING)\n#define LC3_EXPORT __declspec(dllexport)\n#elif defined(_WIN32)\n#define LC3_EXPORT ${lc3_user_decl}\n"
)
vcpkg_replace_string("${SOURCE_PATH}/src/meson.build"
    "gnu_symbol_visibility: 'hidden',"
    "gnu_symbol_visibility: 'hidden',\n\t\tc_args: ['-DLC3_BUILDING'],"
)

vcpkg_configure_meson(
    SOURCE_PATH "${SOURCE_PATH}"
    OPTIONS
        -Dtools=false
        -Dpython=false
)
vcpkg_install_meson()
vcpkg_copy_pdbs()
vcpkg_fixup_pkgconfig()

vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/LICENSE")
