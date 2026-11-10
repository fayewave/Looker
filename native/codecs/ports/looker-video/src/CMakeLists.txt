cmake_minimum_required(VERSION 3.20)
project(looker_video C)

# FFMPEG_ROOT: vcpkg's installed tree (include\, lib\ with the import libraries of the FFmpeg and libass DLLs).
add_library(looker_video SHARED looker_video.c)
target_include_directories(looker_video PRIVATE "${FFMPEG_ROOT}/include")
# libass (ass.dll, with FreeType, HarfBuzz and FriBidi beside it) draws ASS/SSA subtitles.
foreach(lib avformat avcodec avfilter swscale swresample avutil ass)
    find_library(${lib}_LIB NAMES ${lib} PATHS "${FFMPEG_ROOT}/lib" NO_DEFAULT_PATH REQUIRED)
    target_link_libraries(looker_video PRIVATE ${${lib}_LIB})
endforeach()
install(TARGETS looker_video RUNTIME DESTINATION bin LIBRARY DESTINATION lib ARCHIVE DESTINATION lib)
