# CMake toolchain file for WASIX via wasixcc.
#
# wasixcc ships no toolchain file of its own (0.4.7); it is a clang driver
# wrapper that already injects --target, --sysroot, -pthread and the wasm
# feature flags, so all CMake needs is the tool names and a system name.
#
# CMAKE_SYSTEM_NAME=WASI is a platform CMake knows (Modules/Platform/WASI*.cmake,
# it sets the variable WASI=1 and nothing else: UNIX is NOT set).

set(CMAKE_SYSTEM_NAME WASI)
set(CMAKE_SYSTEM_VERSION 1)
set(CMAKE_SYSTEM_PROCESSOR wasm32)

if(DEFINED ENV{WASIXCC_BIN})
  set(_wasixcc_bin "$ENV{WASIXCC_BIN}")
else()
  set(_wasixcc_bin "$ENV{HOME}/.wasixcc/bin")
endif()

set(CMAKE_C_COMPILER   "${_wasixcc_bin}/wasixcc")
set(CMAKE_CXX_COMPILER "${_wasixcc_bin}/wasix++")
set(CMAKE_AR           "${_wasixcc_bin}/wasixar"     CACHE FILEPATH "archiver")
set(CMAKE_RANLIB       "${_wasixcc_bin}/wasixranlib" CACHE FILEPATH "ranlib")
set(CMAKE_NM           "${_wasixcc_bin}/wasixnm"     CACHE FILEPATH "nm")

# Find headers/libraries only in the WASIX sysroot, programs only on the host.
execute_process(
  COMMAND "${_wasixcc_bin}/wasixccenv" print-sysroot
  OUTPUT_VARIABLE _wasix_sysroot
  OUTPUT_STRIP_TRAILING_WHITESPACE
  ERROR_QUIET)
if(_wasix_sysroot)
  set(CMAKE_FIND_ROOT_PATH "${_wasix_sysroot}")
endif()
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)

# Executables are WebAssembly modules; give them the conventional suffix so
# `wasmer run foo.wasm` works on the build outputs.
set(CMAKE_EXECUTABLE_SUFFIX ".wasm")
set(CMAKE_EXECUTABLE_SUFFIX_C ".wasm")
set(CMAKE_EXECUTABLE_SUFFIX_CXX ".wasm")

# The results cannot be executed by the host directly.
set(CMAKE_CROSSCOMPILING TRUE)
