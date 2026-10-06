# LibRaw for Looker: raw_r.dll stays a DLL (LGPL-2.1, replaceable), its dependencies (jasper, lcms, zlib) are
# linked into it, so it's one file. C runtime linked in, release only, as x64-windows-looker. A triplet of its own
# so the HEIC/AVIF build (which shares no package with it) isn't rebuilt.
set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE static)
set(VCPKG_BUILD_TYPE release)
if(PORT STREQUAL "libraw")
    set(VCPKG_LIBRARY_LINKAGE dynamic)
else()
    set(VCPKG_LIBRARY_LINKAGE static)
endif()
