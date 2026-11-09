# looker_video.dll: Looker's own entry points into FFmpeg (src/looker_video.c), linked against the FFmpeg DLLs of
# the overlay ffmpeg port. Built from a copy in the buildtree (the port lives in Dropbox, under a path with spaces).
set(VCPKG_POLICY_EMPTY_INCLUDE_FOLDER enabled)
set(SOURCE_PATH "${CURRENT_BUILDTREES_DIR}/src/looker-video")
file(REMOVE_RECURSE "${SOURCE_PATH}")
file(COPY "${CMAKE_CURRENT_LIST_DIR}/src/" DESTINATION "${SOURCE_PATH}")

vcpkg_cmake_configure(
    SOURCE_PATH "${SOURCE_PATH}"
    OPTIONS "-DFFMPEG_ROOT=${CURRENT_INSTALLED_DIR}"
)
vcpkg_cmake_install()
vcpkg_copy_pdbs()

file(WRITE "${CURRENT_PACKAGES_DIR}/share/${PORT}/copyright" "Part of Looker. FFmpeg (LGPL-2.1-or-later) is linked dynamically.\n")
