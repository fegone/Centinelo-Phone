# core/modules/ctrl_json/CMakeLists.txt
#
# Out-of-tree baresip "application" module, built via baresip's own
# APP_MODULES / APP_MODULES_DIR mechanism (see core/deps/baresip/CMakeLists.txt,
# the `foreach(mod IN LISTS APP_MODULES) add_subdirectory(...)` loop). The
# parent build already appends this target's name to MODULES_DETECTED and
# applies the PREFIX/LINK_FLAGS/symlink treatment uniformly, so this file
# only needs to declare the library itself - same minimal shape as the
# in-tree modules (e.g. modules/g711/CMakeLists.txt, modules/ausine/CMakeLists.txt).

project(ctrl_json)

set(SRCS ctrl_json.c cmd.c dialog_info.c wav_writer.c audiotap.c)

if(STATIC)
  add_library(${PROJECT_NAME} OBJECT ${SRCS})
else()
  add_library(${PROJECT_NAME} MODULE ${SRCS})
endif()
