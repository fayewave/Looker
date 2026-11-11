# Looker's MSVC build of libzvbi (upstream is autotools for Linux and MinGW). Only the Teletext page decoder that
# FFmpeg's libzvbi_teletext decoder uses (libavcodec/libzvbi-teletextdec.c: vbi_decoder_new/_delete,
# vbi_event_handler_register, vbi_decode, vbi_fetch_vt_page, vbi_unref_page, vbi_print_page_region,
# vbi_draw_vt_page_region, vbi_teletext_set_default_region, vbi_classify_page, vbi_version and the inline
# Hamming helpers), with what those pull in. No capture devices, proxy, raw decoder, demuxers or PNG export.
#
# Licences (src/*): everything compiled here is LGPL-2.0+ (ure.c MIT, exp-templ.c no-notice template). The
# library's GPL-2 files are left out and removed from the tree by the portfile: packet-830.c/.h and pdc.c/.h
# (replaced by win32/looker_830.c stubs and headers generated below from libzvbi.h, which upstream ships as
# LGPL) and exp-vtx.c (upstream doesn't build it either).
cmake_minimum_required(VERSION 3.20)
project(zvbi VERSION 0.2.46 LANGUAGES C)

set(UPSTREAM "${CMAKE_CURRENT_SOURCE_DIR}/src")
set(GEN "${CMAKE_CURRENT_BINARY_DIR}/gen")
set(SRC "${GEN}/src")

# zvbi is written in GNU C. msvc.patch fixes the few one-off spots; MSVC then compiles a copy of src/ with the
# mechanical rewrites: case ranges ("case 0x10 ... 0x17:", numbers only) spelled out, named variadic macros
# ("templ, args..." / "##args") turned into __VA_ARGS__, and the logging macro warning() renamed. The GPL files
# are never copied.
file(GLOB upstream_files "${UPSTREAM}/*.c" "${UPSTREAM}/*.h" "${UPSTREAM}/*.xbm")
foreach(path IN LISTS upstream_files)
    get_filename_component(name "${path}" NAME)
    if(name MATCHES "^(packet-830|pdc)\\.[ch]$|^exp-vtx\\.c$")
        continue()
    endif()
    file(READ "${path}" text)
    while(text MATCHES "case[ \t]+(0x[0-9A-Fa-f]+|[0-9]+)[ \t]*\\.\\.\\.[ \t]*(0x[0-9A-Fa-f]+|[0-9]+)[ \t]*:")
        set(range "${CMAKE_MATCH_0}")
        math(EXPR lo "${CMAKE_MATCH_1}")
        math(EXPR hi "${CMAKE_MATCH_2}")
        set(cases "")
        foreach(v RANGE ${lo} ${hi})
            string(APPEND cases "case ${v}: ")
        endforeach()
        string(REPLACE "${range}" "${cases}" text "${text}")
    endwhile()
    string(REPLACE "args...)" "...)" text "${text}")
    string(REPLACE "##args" "##__VA_ARGS__" text "${text}")
    # misc.h's logging macro warning() would also catch the SDK headers' "#pragma warning(push)".
    string(REGEX REPLACE "([^A-Za-z0-9_])warning[ \t]*\\(" "\\1zvbi_warning(" text "${text}")
    file(WRITE "${SRC}/${name}" "${text}")
endforeach()

set(ZVBI_SOURCES
    caption.c       # Closed Caption page formatting (vbi.c dispatches to it)
    cache.c         # Teletext page cache
    conv.c          # UCS-2 / iconv helpers
    event.c         # event handler lists
    exp-gfx.c       # vbi_draw_vt_page_region, PPM/XPM export (PNG left out: no HAVE_LIBPNG)
    exp-html.c      # export module table (export.c)
    exp-templ.c
    exp-txt.c       # vbi_print_page_region
    export.c
    hamm.c          # Hamming / parity tables
    lang.c          # Teletext character sets
    misc.c
    packet.c        # Teletext packet decoding
    page_table.c
    search.c        # vbi_search_* (vbi.c, cache.c)
    ure.c           # regex engine for search.c (MIT)
    tables.c        # rating / programme type strings
    teletext.c      # vbi_fetch_vt_page, page formatting
    trigger.c       # EACEM triggers (vbi.c)
    vbi.c           # vbi_decoder_new/delete, vbi_decode
    vps.c           # VPS decoding (packet.c)
    wss.c           # WSS decoding (vbi.c)
)
list(TRANSFORM ZVBI_SOURCES PREPEND "${SRC}/")

# Headers to stand in for the GPL pdc.h and packet-830.h: their public parts as upstream's LGPL libzvbi.h carries
# them (types, enums and prototypes; nothing compiled from them).
file(READ "${SRC}/libzvbi.h" ZVBI_H)
function(zvbi_section name next out)
    string(FIND "${ZVBI_H}" "/* ${name} */\n" a)
    string(FIND "${ZVBI_H}" "/* ${next} */\n" b)
    if(a LESS 0 OR b LESS a)
        message(FATAL_ERROR "libzvbi.h: section ${name} not found")
    endif()
    string(LENGTH "/* ${name} */\n" n)
    math(EXPR a "${a} + ${n}")
    math(EXPR len "${b} - ${a}")
    string(SUBSTRING "${ZVBI_H}" ${a} ${len} s)
    set(${out} "${s}" PARENT_SCOPE)
endfunction()
zvbi_section(pdc.h event.h PDC_PUBLIC)
zvbi_section(packet-830.h vps.h P830_PUBLIC)
file(WRITE "${GEN}/pdc.h" "/* Looker: pdc.h's public part, from libzvbi.h (LGPL). */
#ifndef __ZVBI_PDC_H__
#define __ZVBI_PDC_H__
#include <stdio.h>
#include <inttypes.h>
#include <time.h>
#include \"version.h\"
#include \"macros.h\"
#include \"bcd.h\"
#include \"network.h\"
VBI_BEGIN_DECLS
${PDC_PUBLIC}
VBI_END_DECLS
#endif
")
file(WRITE "${GEN}/packet-830.h" "/* Looker: packet-830.h's public part, from libzvbi.h (LGPL). */
#ifndef __ZVBI_PACKET_830_H__
#define __ZVBI_PACKET_830_H__
#include <inttypes.h>
#include <time.h>
#include \"macros.h\"
#include \"pdc.h\"
VBI_BEGIN_DECLS
${P830_PUBLIC}
VBI_END_DECLS
#endif
")
file(WRITE "${GEN}/site_def.h" "/* Site specific definitions (none) */\n")
file(WRITE "${GEN}/config.h" "/* Looker: what configure would find with MSVC. Force-included in every file. */
#ifndef LOOKER_ZVBI_CONFIG_H
#define LOOKER_ZVBI_CONFIG_H
#define _CRT_SECURE_NO_WARNINGS 1
#define _CRT_NONSTDC_NO_DEPRECATE 1
#define PACKAGE \"zvbi\"
#define VERSION \"${PROJECT_VERSION}\"
#define HAVE_ICONV 1
#define Z_LITTLE_ENDIAN 1234
#define Z_BIG_ENDIAN 4321
#define Z_BYTE_ORDER 1234
#include <basetsd.h>
typedef SSIZE_T ssize_t;
typedef int mode_t;
#define SSIZE_MAX INTPTR_MAX
#define __PRETTY_FUNCTION__ __FUNCTION__
#define __inline__ __inline
#define S_IRUSR _S_IREAD
#define S_IWUSR _S_IWRITE
#define S_IRGRP 0
#define S_IWGRP 0
#define S_IROTH 0
#define S_IWOTH 0
#define S_ISREG(m) (((m) & _S_IFMT) == _S_IFREG)
#define strcasecmp _stricmp
#define strncasecmp _strnicmp
/* misc.h defines a function-like macro error(): read the system headers before it can get in the way. */
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <winsock2.h>
#include <windows.h>
#include <assert.h>
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <float.h>
#include <inttypes.h>
#include <io.h>
#include <limits.h>
#include <locale.h>
#include <math.h>
#include <process.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <wchar.h>
#endif
")

add_library(zvbi
    ${ZVBI_SOURCES}
    win32/looker_iconv.c
    win32/looker_830.c
    win32/zvbi-data.def
)
target_include_directories(zvbi PRIVATE "${GEN}" "${CMAKE_CURRENT_SOURCE_DIR}/win32" "${SRC}")
target_compile_definitions(zvbi PRIVATE HAVE_CONFIG_H)
# The sources mix Latin-1 and UTF-8 string literals: read and write them as ISO-8859-1, which keeps every byte as
# it is (what GCC does). vcpkg's toolchain adds /utf-8, which cl refuses alongside.
string(REPLACE "/utf-8" "" CMAKE_C_FLAGS "${CMAKE_C_FLAGS}")
target_compile_options(zvbi PRIVATE "/FI${GEN}/config.h" /source-charset:.28591 /execution-charset:.28591
    /wd4244 /wd4267 /wd4018 /wd4996 /wd4146)
set_target_properties(zvbi PROPERTIES
    C_STANDARD 11
    WINDOWS_EXPORT_ALL_SYMBOLS ON
    RUNTIME_OUTPUT_NAME zvbi-0      # zvbi-0.dll, libtool's name minus the "lib"
    ARCHIVE_OUTPUT_NAME zvbi        # zvbi.lib: -lzvbi as zvbi-0.2.pc says
)

# The installed libzvbi.h: upstream's, minus the sections for code not built here (the DVB/IDL/PFC/XDS demuxers
# and muxer; capture and proxy, which need sys/time.h; packet 8/30), plus ssize_t for MSVC and dllimport on the
# Hamming tables the inline helpers read. (The raw decoder's declarations stay, unbuilt, for decoder.h's types.)
function(zvbi_cut from to)
    string(FIND "${ZVBI_H}" "/* ${from} */\n" a)
    string(FIND "${ZVBI_H}" "/* ${to} */\n" b)
    if(a LESS 0 OR b LESS a)
        message(FATAL_ERROR "libzvbi.h: section ${from} not found")
    endif()
    string(SUBSTRING "${ZVBI_H}" 0 ${a} head)
    string(SUBSTRING "${ZVBI_H}" ${b} -1 tail)
    set(ZVBI_H "${head}${tail}" PARENT_SCOPE)
endfunction()
zvbi_cut(dvb_demux.h exp-gfx.h)
zvbi_cut(packet-830.h vps.h)
# decoder.h stays (vbi_pixfmt for vbi_draw_vt_page_region); its raw decoder struct holds a mutex, a pointer-sized
# SRWLOCK in this build.
string(REPLACE "#include <pthread.h>\n" "" ZVBI_H "${ZVBI_H}")
string(REGEX REPLACE "pthread_mutex_t([\t ]+)mutex;" "void *\\1mutex; /* Looker: pthread_mutex_t, an SRWLOCK */"
    ZVBI_H "${ZVBI_H}")
string(REPLACE "#define __LIBZVBI_H__\n" "#define __LIBZVBI_H__\n
#if defined(_MSC_VER) && !defined(_SSIZE_T_DEFINED)
#include <basetsd.h>
typedef SSIZE_T ssize_t;
#define _SSIZE_T_DEFINED
#endif
#if defined(_WIN32) && !defined(ZVBI_STATIC)
#define ZVBI_DATA __declspec(dllimport)
#else
#define ZVBI_DATA
#endif
" ZVBI_H "${ZVBI_H}")
if(NOT BUILD_SHARED_LIBS)
    string(REPLACE "!defined(ZVBI_STATIC)" "0" ZVBI_H "${ZVBI_H}")
endif()
string(REGEX REPLACE "extern const (u?int8_t[\t ]+_vbi_)" "extern ZVBI_DATA const \\1" ZVBI_H "${ZVBI_H}")
file(WRITE "${GEN}/install/libzvbi.h" "${ZVBI_H}")

set(prefix "${CMAKE_INSTALL_PREFIX}")
set(exec_prefix "\${prefix}")
set(libdir "\${prefix}/lib")
set(includedir "\${prefix}/include")
set(VERSION "${PROJECT_VERSION}")
set(LIBS "")
configure_file("${CMAKE_CURRENT_SOURCE_DIR}/zvbi-0.2.pc.in" "${GEN}/zvbi-0.2.pc" @ONLY)
file(READ "${GEN}/zvbi-0.2.pc" PC)
string(REPLACE " -lm" "" PC "${PC}")
file(WRITE "${GEN}/zvbi-0.2.pc" "${PC}")

install(TARGETS zvbi RUNTIME DESTINATION bin LIBRARY DESTINATION lib ARCHIVE DESTINATION lib)
install(FILES "${GEN}/install/libzvbi.h" DESTINATION include)
install(FILES "${GEN}/zvbi-0.2.pc" DESTINATION lib/pkgconfig)
