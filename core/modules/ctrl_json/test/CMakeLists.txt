# core/modules/ctrl_json/test/CMakeLists.txt
#
# Standalone CMake project for ctrl_json's unit tests (cmd.c +
# dialog_info.c - see ../cmd.h / ../dialog_info.h for why these two files
# are pure/parseable and testable without a running baresip engine).
#
# Deliberately NOT folded into the ctrl_json module's own CMakeLists.txt
# (the one baresip's build processes via APP_MODULES/add_subdirectory,
# see ../CMakeLists.txt): that file lives inside baresip's own enormous
# CMake tree, where a nested project()+enable_testing() several
# directories deep is not reliably discovered by `ctest` run from
# baresip's own build dir. This is instead a tiny, independent CMake
# project with its own build dir - `cmake -S . -B build && cmake --build
# build && ctest --test-dir build` just works - matching core/BUILD.md's
# own "exact, from-clean-clone" command style.
#
# Links the SAME core/deps/re build the engine itself uses (same pinned
# v4.9.0 sources + local patches, see ../../../patches/), not a fresh or
# system libre, so what's unit tested matches what's actually shipped.
# Requires that build to already exist - see core/BUILD.md step 4a.

cmake_minimum_required(VERSION 3.19)
project(ctrl_json_test C)

set(CMAKE_C_STANDARD 99)
set(CMAKE_C_STANDARD_REQUIRED ON)

set(RE_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../../deps/re" CACHE PATH
	"Path to the core/deps/re submodule (must already be built, see core/BUILD.md step 4a)")

set(RE_STATIC_LIB "${RE_ROOT}/build/libre.a")

if(NOT EXISTS "${RE_ROOT}/include/re.h" OR NOT EXISTS "${RE_STATIC_LIB}")
	message(FATAL_ERROR
		"core/deps/re is not built (looked for ${RE_STATIC_LIB}). "
		"Run core/BUILD.md step 4a first:\n"
		"  cmake -S core/deps/re -B core/deps/re/build -DCMAKE_BUILD_TYPE=Release\n"
		"  cmake --build core/deps/re/build\n"
		"then re-configure this test project.")
endif()

# re is built with USE_OPENSSL (see core/BUILD.md) - libre_init() calls
# openssl_init() unconditionally in that configuration, so even these
# TLS-free unit tests need OpenSSL at link time. Mirrors
# core/deps/re/build/libre.pc's own Libs.private/Requires.private for
# this exact build, rather than guessing.
find_package(OpenSSL REQUIRED)
find_package(ZLIB)

add_executable(ctrl_json_test
	test_main.c
	../cmd.c
	../dialog_info.c)

target_include_directories(ctrl_json_test PRIVATE "${RE_ROOT}/include")

target_link_libraries(ctrl_json_test PRIVATE
	"${RE_STATIC_LIB}"
	OpenSSL::SSL
	OpenSSL::Crypto)

if(ZLIB_FOUND)
	target_link_libraries(ctrl_json_test PRIVATE ZLIB::ZLIB)
endif()

if(APPLE)
	target_link_libraries(ctrl_json_test PRIVATE
		resolv m
		"-framework SystemConfiguration"
		"-framework CoreFoundation")
elseif(UNIX)
	target_link_libraries(ctrl_json_test PRIVATE resolv m pthread)
endif()

option(CENT_ASAN "Build ctrl_json_test with AddressSanitizer" OFF)
if(CENT_ASAN)
	target_compile_options(ctrl_json_test PRIVATE
		-fsanitize=address -fno-omit-frame-pointer -g -O0)
	target_link_options(ctrl_json_test PRIVATE -fsanitize=address)
endif()

enable_testing()
add_test(NAME ctrl_json_test COMMAND ctrl_json_test)
