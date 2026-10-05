# Looker's codec DLLs: dynamic libraries (LGPL libheif/libde265 must stay replaceable) with the C runtime linked
# in (a Store install can't count on the Visual C++ redistributable), release only.
set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE static)
set(VCPKG_LIBRARY_LINKAGE dynamic)
set(VCPKG_BUILD_TYPE release)
