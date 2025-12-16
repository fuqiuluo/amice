project(amice-fmt)
cmake_minimum_required(VERSION 3.4...3.27)

add_executable(amice-fmt main.cpp)
target_link_libraries(amice-fmt fmt-header-only)
target_compile_options(amice-fmt PRIVATE -fpass-plugin=$ENV{AMICE_PATH})

add_subdirectory(fmt)
